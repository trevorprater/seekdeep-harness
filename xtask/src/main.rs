//! Repository gates that keep the source inventory and Rust parity evidence synchronized.

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    time::{Duration, UNIX_EPOCH},
};

use base64::Engine as _;
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};

mod client_test_runtime_built_smoke_driver;
mod remote_built_smoke_driver;
mod remote_contracts;

#[derive(Debug, Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Verify the complete generated-Remote browser-to-Host milestone.
    RemoteMilestone {
        #[arg(long, default_value = "/Users/trevor/ws/deepseek-harness")]
        source: PathBuf,
    },
    /// Compare complete built Remote metadata and codec behavior with the pinned emitter.
    RemoteCodecOracle {
        #[arg(long, default_value = "/Users/trevor/ws/deepseek-harness")]
        source: PathBuf,
    },
    /// Run the pinned Client gateway corpus with the real browser WASM registry.
    RemoteGatewayOracle {
        #[arg(long, default_value = "/Users/trevor/ws/deepseek-harness")]
        source: PathBuf,
        /// Validate the supplemental lifecycle cases on the source implementation.
        #[arg(long)]
        source_regressions: bool,
    },
    /// Verify the generated Remote path in Chromium against the real built Rust Host.
    RemoteBrowserPath {
        #[arg(long, default_value = "/Users/trevor/ws/deepseek-harness")]
        source: PathBuf,
    },
    /// Run the pinned registry corpus against the built browser WASM implementation.
    RemoteRegistryOracle {
        #[arg(long, default_value = "/Users/trevor/ws/deepseek-harness")]
        source: PathBuf,
    },
    /// Generate browser Remote construction plans through the Rust Typert emitter.
    RemoteContracts {
        /// Refresh the contract model from this read-only pinned oracle.
        #[arg(long)]
        capture_source: Option<PathBuf>,
        /// Verify the existing generated plan without writing it.
        #[arg(long)]
        check: bool,
    },
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
    /// Build and drive generated Goal Remotes through built WASM and a real Rust Host.
    RemoteBuiltSmoke,
    /// Build and import the Client test runtime through real Vitest, React, and jsdom.
    ClientTestRuntimeBuiltSmoke {
        /// Pinned source checkout supplying the oracle's installed JavaScript test dependencies.
        #[arg(long, default_value = "/Users/trevor/ws/deepseek-harness")]
        source: PathBuf,
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
    /// Generate the JavaScript entry and Vite configuration for the Rust/WASM Web shell.
    WebFrontend,
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
        Command::RemoteMilestone { source } => remote_contracts::milestone(&source),
        Command::RemoteCodecOracle { source } => remote_contracts::codec_oracle(&source),
        Command::RemoteGatewayOracle {
            source,
            source_regressions,
        } => remote_contracts::gateway_oracle(&source, source_regressions),
        Command::RemoteBrowserPath { source } => remote_contracts::browser_path(&source),
        Command::RemoteRegistryOracle { source } => remote_contracts::registry_oracle(&source),
        Command::RemoteContracts {
            capture_source,
            check,
        } => remote_contracts::run(capture_source.as_deref(), check),
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
        Command::RemoteBuiltSmoke => remote_built_smoke(),
        Command::ClientTestRuntimeBuiltSmoke { source } => client_test_runtime_built_smoke(&source),
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
        Command::WebFrontend => web_frontend(),
        Command::WasmPackage {
            package,
            artifact,
            module_id,
            out_dir,
            watch,
        } => wasm_package(&package, &artifact, &module_id, &out_dir, watch),
    }
}

const WEB_FRONTEND_ENTRY: &str = r"/** Generated mount binding for the compiled Rust/WASM Web shell. */
import { AppWebEntry } from '@seekdeep-ai/seekdeep-client-web'

const root = document.getElementById('root')
if (root === null) throw new Error('web app: missing #root')
void new AppWebEntry(root).run()
";

const WEB_FRONTEND_VITE_CONFIG: &str = r#"/** Generated by `cargo xtask web-frontend`; edit xtask/src/main.rs. */
import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

const STANDALONE_ERROR =
  'apps/web is not a standalone application: bare Vite cannot inject window.__SEEKDEEP_BOOT__. '
  + 'From a repository checkout, run `pnpm seekdeep web`; an installed package uses `seekdeep web`. '
  + 'For client-plugin HMR, run `pnpm seekdeep web` together with `pnpm run dev:web`.'

function rejectStandaloneServe() {
  return {
    name: 'seekdeep-reject-standalone-web-serve',
    config(_config, environment) {
      if (environment.command === 'serve') throw new Error(STANDALONE_ERROR)
    },
  }
}

const VENDOR_PACKAGES = new Set([
  'katex',
  'shiki',
  'mdast-util-from-markdown',
  'mdast-util-gfm',
  'mdast-util-math',
  'micromark-core-commonmark',
  'micromark-extension-gfm',
  'micromark-extension-math',
  'micromark-factory-space',
  'micromark-util-character',
  'micromark-util-classify-character',
  'micromark-util-sanitize-uri',
  'micromark-util-symbol',
  'micromark-util-types',
])

const BOOT_GRAMMAR_FILES = [
  'dist/typescript.mjs',
  'dist/shellscript.mjs',
  'dist/json.mjs',
]

const FONT_EXTENSIONS = ['.woff2', '.woff', '.ttf']

function npmPackageOf(id) {
  const parts = id.split('/node_modules/')
  if (parts.length === 1) return undefined
  const [first, second] = parts[parts.length - 1].split('/')
  if (first.startsWith('.')) return undefined
  if (first.startsWith('@')) return second === undefined ? undefined : `${first}/${second}`
  return first
}

export default defineConfig({
  plugins: [rejectStandaloneServe(), react()],
  build: {
    target: 'esnext',
    sourcemap: true,
    rollupOptions: {
      output: {
        chunkFileNames(chunk) {
          if (chunk.name === 'index' || chunk.name === 'vendor') {
            return 'assets/[name]-[hash].js'
          }
          const grammar = chunk.moduleIds
            .some(id => id.includes('/node_modules/@shikijs/langs/'))
          return grammar
            ? 'assets/langs/[name]-[hash].js'
            : 'assets/[name]-[hash].js'
        },
        assetFileNames(asset) {
          const name = asset.names[0] ?? ''
          return FONT_EXTENSIONS.some(extension => name.endsWith(extension))
            ? 'assets/fonts/[name]-[hash][extname]'
            : 'assets/[name]-[hash][extname]'
        },
        manualChunks(id) {
          const packageName = npmPackageOf(id)
          if (packageName === undefined) return undefined
          if (packageName === '@shikijs/langs') {
            return BOOT_GRAMMAR_FILES.some(file => id.endsWith(`/${file}`))
              ? 'vendor'
              : undefined
          }
          return VENDOR_PACKAGES.has(packageName) ? 'vendor' : undefined
        },
      },
    },
  },
  define: {
    'process.versions.node': '"0.0.0"',
    'process.execArgv': '[]',
    'process.env.CORDIS_SHARED': 'undefined',
  },
})
"#;

fn web_frontend() -> anyhow::Result<()> {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| anyhow::anyhow!("xtask has no workspace parent"))?;
    write_web_frontend(workspace_root)
}

fn write_web_frontend(workspace_root: &Path) -> anyhow::Result<()> {
    let output = workspace_root.join("apps/web/generated");
    std::fs::create_dir_all(&output)?;
    write_generated_file(&output.join("main.js"), WEB_FRONTEND_ENTRY)?;
    write_generated_file(&output.join("vite.config.mjs"), WEB_FRONTEND_VITE_CONFIG)?;
    Ok(())
}

fn write_generated_file(path: &Path, contents: &str) -> anyhow::Result<()> {
    if std::fs::read_to_string(path).is_ok_and(|current| current == contents) {
        return Ok(());
    }
    std::fs::write(path, contents)?;
    Ok(())
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

fn remote_built_smoke() -> anyhow::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a workspace parent");
    let status = ProcessCommand::new("cargo")
        .current_dir(root)
        .env("CARGO_BUILD_JOBS", "2")
        .env("CARGO_INCREMENTAL", "0")
        .args(["build", "-p", "seekdeep"])
        .status()?;
    anyhow::ensure!(status.success(), "native SeekDeep Host build failed");

    for (package, artifact, module_id, out_dir) in [
        (
            "seekdeep-cordis",
            "seekdeep_cordis",
            "@seekdeep-ai/cordis",
            "vendor/cordis/lib",
        ),
        (
            "seekdeep-client-foundation-wasm",
            "seekdeep_client_foundation_wasm",
            "@seekdeep-ai/seekdeep-client-connection",
            "packages/client/connection/lib",
        ),
        (
            "seekdeep-client-foundation-wasm",
            "seekdeep_client_foundation_wasm",
            "@seekdeep-ai/seekdeep-typert-registry",
            "packages/typert/registry/lib",
        ),
        (
            "seekdeep-client-foundation-wasm",
            "seekdeep_client_foundation_wasm",
            "@seekdeep-ai/seekdeep-api-gateway",
            "packages/api/gateway/lib",
        ),
        (
            "seekdeep-api-remotes-client",
            "seekdeep_api_remotes_client",
            "@seekdeep-ai/seekdeep-api-remotes",
            "packages/api/remotes/lib",
        ),
    ] {
        wasm_package_once(package, artifact, module_id, &root.join(out_dir))?;
    }

    let node = std::env::var_os("npm_node_execpath").unwrap_or_else(|| "node".into());
    let driver_dir = root.join("target/xtask/remote-built-smoke");
    std::fs::create_dir_all(&driver_dir)?;
    let driver = driver_dir.join("built_remote_chain.mjs");
    std::fs::write(&driver, remote_built_smoke_driver::DRIVER)?;
    let status = ProcessCommand::new(node)
        .current_dir(root)
        .arg(&driver)
        .status()?;
    std::fs::remove_file(driver)?;
    anyhow::ensure!(status.success(), "built Goal Remote chain failed");
    Ok(())
}

fn client_test_runtime_built_smoke(source: &Path) -> anyhow::Result<()> {
    verify_source(source)?;
    let metadata = cargo_metadata()?;
    let root = &metadata.workspace_root;
    wasm_package_once(
        "seekdeep-client-test-runtime",
        "seekdeep_client_test_runtime",
        "@seekdeep-ai/seekdeep-client-test-runtime",
        &root.join("packages/test-support/client-runtime/lib"),
    )?;

    let source_package = source.join("packages/test-support/client-runtime");
    let smoke_dir = metadata
        .target_directory
        .join("xtask/client-test-runtime-built-smoke");
    if smoke_dir.exists() {
        std::fs::remove_dir_all(&smoke_dir)?;
    }
    std::fs::create_dir_all(&smoke_dir)?;
    let config = client_test_runtime_vitest_config(source, &source_package, &smoke_dir)?;
    let test = client_test_runtime_built_smoke_driver::TEST.replace(
        "__RUNTIME_MODULE__",
        &quoted_path(&root.join("packages/test-support/client-runtime/lib/index.js"))?,
    );
    let config_path = smoke_dir.join("vitest.config.mjs");
    let typecheck_path = smoke_dir.join("typecheck.ts");
    let tsconfig_path = smoke_dir.join("tsconfig.json");
    let tsconfig = client_test_runtime_tsconfig(root, &source_package, &typecheck_path)?;
    std::fs::write(&config_path, config)?;
    std::fs::write(smoke_dir.join("runtime.test.mjs"), test)?;
    std::fs::write(
        &typecheck_path,
        client_test_runtime_built_smoke_driver::TYPECHECK,
    )?;
    std::fs::write(&tsconfig_path, tsconfig)?;

    let vitest = source.join("node_modules/.bin/vitest");
    anyhow::ensure!(
        vitest.is_file(),
        "pinned source has no installed Vitest binary at {}",
        vitest.display()
    );
    let status = ProcessCommand::new(vitest)
        .current_dir(root)
        .args(["run", "--config"])
        .arg(config_path)
        .status()?;
    anyhow::ensure!(status.success(), "built Client test runtime smoke failed");
    let tsc = source.join("node_modules/.bin/tsc");
    anyhow::ensure!(
        tsc.is_file(),
        "pinned source has no installed TypeScript compiler at {}",
        tsc.display()
    );
    let status = ProcessCommand::new(tsc)
        .current_dir(root)
        .args(["-p"])
        .arg(tsconfig_path)
        .status()?;
    anyhow::ensure!(
        status.success(),
        "built Client test runtime declaration smoke failed"
    );
    Ok(())
}

fn client_test_runtime_vitest_config(
    source: &Path,
    source_package: &Path,
    smoke_dir: &Path,
) -> anyhow::Result<String> {
    Ok(client_test_runtime_built_smoke_driver::CONFIG
        .replace(
            "__REACT__",
            &quoted_path(&source_package.join("node_modules/react/index.js"))?,
        )
        .replace(
            "__REACT_DOM__",
            &quoted_path(&source_package.join("node_modules/react-dom/index.js"))?,
        )
        .replace(
            "__TESTING_REACT__",
            &quoted_path(
                &source_package.join("node_modules/@testing-library/react/dist/index.js"),
            )?,
        )
        .replace(
            "__TESTING_DOM__",
            &quoted_path(&source_package.join("node_modules/@testing-library/dom/dist/index.js"))?,
        )
        .replace(
            "__VITEST__",
            &quoted_path(&source_package.join("node_modules/vitest/dist/index.js"))?,
        )
        .replace(
            "__IMMER__",
            &quoted_path(
                &source.join("packages/client/runtime/node_modules/immer/dist/immer.mjs"),
            )?,
        )
        .replace(
            "__TEST_FILE__",
            &quoted_path(&smoke_dir.join("runtime.test.mjs"))?,
        ))
}

fn client_test_runtime_tsconfig(
    root: &Path,
    source_package: &Path,
    typecheck_path: &Path,
) -> anyhow::Result<String> {
    Ok(client_test_runtime_built_smoke_driver::TSCONFIG
        .replace("__ROOT__", &quoted_path(root)?)
        .replace(
            "__CORDIS_TYPES__",
            &quoted_path(&root.join("vendor/cordis/lib/types/index.d.ts"))?,
        )
        .replace(
            "__RUNTIME_TYPES__",
            &quoted_path(&root.join("packages/client/runtime/lib/types/client/index.d.ts"))?,
        )
        .replace(
            "__SLOT_TYPES__",
            &quoted_path(&root.join("packages/client/ui-slots/lib/types/index.d.ts"))?,
        )
        .replace(
            "__TEST_RUNTIME_TYPES__",
            &quoted_path(&root.join("packages/test-support/client-runtime/lib/types/index.d.ts"))?,
        )
        .replace(
            "__TESTING_DOM_TYPES__",
            &quoted_path(
                &source_package.join("node_modules/@testing-library/dom/types/index.d.ts"),
            )?,
        )
        .replace(
            "__TESTING_REACT_TYPES__",
            &quoted_path(
                &source_package.join("node_modules/@testing-library/react/types/index.d.ts"),
            )?,
        )
        .replace(
            "__REACT_TYPES__",
            &quoted_path(&source_package.join("node_modules/@types/react/index.d.ts"))?,
        )
        .replace(
            "__VITEST_TYPES__",
            &quoted_path(&source_package.join("node_modules/vitest/dist/index.d.ts"))?,
        )
        .replace("__TYPECHECK_FILE__", &quoted_path(typecheck_path)?))
}

fn quoted_path(path: &Path) -> anyhow::Result<String> {
    Ok(serde_json::to_string(&path.to_string_lossy())?)
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
        "@seekdeep-ai/seekdeep-client-ui-attachment" => Some(
            cargo_metadata()?
                .workspace_root
                .join("packages/client/ui-attachment"),
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
        .env("CARGO_BUILD_JOBS", "2")
        .env("CARGO_INCREMENTAL", "0")
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
    if module_id == "@seekdeep-ai/cordis" {
        return wasm_cordis_package(&metadata, artifact, out_dir, &wasm);
    }
    if module_id == "@seekdeep-ai/cordis-plugin-loader" {
        return wasm_client_loader_package(&metadata, artifact, out_dir, &wasm);
    }
    if module_id == "@seekdeep-ai/seekdeep-client-modules" {
        return wasm_client_modules_package(&metadata, artifact, out_dir, &wasm);
    }
    if matches!(
        module_id,
        "@seekdeep-ai/seekdeep-client-ui-slots"
            | "@seekdeep-ai/seekdeep-client-schema-form"
            | "@seekdeep-ai/seekdeep-client-web-react"
            | "@seekdeep-ai/seekdeep-client-test-runtime"
    ) {
        return wasm_foundation_esm_package(&metadata, artifact, module_id, out_dir, &wasm);
    }
    if module_id == "@seekdeep-ai/seekdeep-client-web" {
        return wasm_web_shell_package(&metadata, artifact, out_dir, &wasm);
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-primitives" {
        return wasm_ui_primitives_package(&metadata, artifact, out_dir, &wasm);
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-attachment" {
        return wasm_ui_attachment_package(&metadata, artifact, out_dir, &wasm);
    }
    wasm_classic_package(&metadata, artifact, module_id, out_dir, &wasm)
}

fn wasm_classic_package(
    metadata: &CargoMetadata,
    artifact: &str,
    module_id: &str,
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
    let global = wasm_package_global(artifact, module_id);
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
        .arg(wasm)
        .status()?;
    anyhow::ensure!(status.success(), "wasm-bindgen failed for {module_id}");
    let bindings = std::fs::read_to_string(staging.join("client.js"))?;
    let bytes = std::fs::read(staging.join("client_bg.wasm"))?;
    let bundle = classic_module_bundle(&bindings, &bytes, &global, module_id)?;
    let bundle = if module_id == "@seekdeep-ai/seekdeep-api-remotes" {
        remote_contracts::bundle_zod(&metadata.workspace_root, &bundle)?
    } else {
        bundle
    };
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

fn wasm_cordis_package(
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
    anyhow::ensure!(status.success(), "wasm-bindgen failed for browser Cordis");
    let out_dir = if out_dir.is_absolute() {
        out_dir.to_owned()
    } else {
        metadata.workspace_root.join(out_dir)
    };
    std::fs::create_dir_all(&out_dir)?;
    for name in ["client.js", "client.d.ts", "client_bg.wasm"] {
        std::fs::copy(staging.join(name), out_dir.join(name))?;
    }
    std::fs::write(out_dir.join("index.js"), cordis_esm_wrapper())?;
    let type_dir = out_dir.join("types");
    if type_dir.exists() {
        std::fs::remove_dir_all(&type_dir)?;
    }
    std::fs::create_dir_all(&type_dir)?;
    std::fs::write(type_dir.join("index.d.ts"), cordis_esm_declarations())?;
    println!(
        "built @seekdeep-ai/cordis Rust/WASM ESM runtime at {}",
        out_dir.join("index.js").display()
    );
    Ok(())
}

fn wasm_client_loader_package(
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
    anyhow::ensure!(status.success(), "wasm-bindgen failed for browser Loader");
    let out_dir = if out_dir.is_absolute() {
        out_dir.to_owned()
    } else {
        metadata.workspace_root.join(out_dir)
    };
    std::fs::create_dir_all(&out_dir)?;
    for name in ["client.js", "client.d.ts", "client_bg.wasm"] {
        std::fs::copy(staging.join(name), out_dir.join(name))?;
    }
    std::fs::write(out_dir.join("index.js"), client_loader_esm_wrapper())?;
    let type_dir = out_dir.join("types");
    if type_dir.exists() {
        std::fs::remove_dir_all(&type_dir)?;
    }
    std::fs::create_dir_all(&type_dir)?;
    std::fs::write(
        type_dir.join("index.d.ts"),
        client_loader_esm_declarations(),
    )?;
    println!(
        "built @seekdeep-ai/cordis-plugin-loader Rust/WASM ESM runtime at {}",
        out_dir.join("index.js").display()
    );
    Ok(())
}

fn wasm_client_modules_package(
    metadata: &CargoMetadata,
    artifact: &str,
    out_dir: &Path,
    wasm: &Path,
) -> anyhow::Result<()> {
    let staging = wasm_bindgen_web_staging(metadata, artifact, wasm, "client modules")?;
    let out_dir = workspace_output_dir(metadata, out_dir);
    std::fs::create_dir_all(&out_dir)?;
    for name in ["wasm.js", "wasm.d.ts", "wasm_bg.wasm"] {
        std::fs::copy(staging.join(name), out_dir.join(name))?;
    }
    std::fs::write(out_dir.join("client.js"), client_modules_esm_wrapper())?;
    std::fs::write(out_dir.join("index.js"), "export * from './client.js';\n")?;
    std::fs::write(
        out_dir.join("invariant.js"),
        "export const name = 'client-modules-invariant';\nexport const inject = ['invariants'];\nexport const apply = () => {};\n",
    )?;
    let type_dir = out_dir.join("types/client");
    std::fs::create_dir_all(&type_dir)?;
    std::fs::write(type_dir.join("index.d.ts"), client_modules_declarations())?;
    std::fs::write(
        out_dir.join("types/index.d.ts"),
        "export * from './client/index.js';\n",
    )?;
    std::fs::write(
        out_dir.join("types/invariant.d.ts"),
        "export declare const name: 'client-modules-invariant';\nexport declare const inject: readonly ['invariants'];\nexport declare function apply(): void;\n",
    )?;
    println!(
        "built @seekdeep-ai/seekdeep-client-modules Rust/WASM ESM client at {}",
        out_dir.join("client.js").display()
    );
    Ok(())
}

fn wasm_foundation_esm_package(
    metadata: &CargoMetadata,
    artifact: &str,
    module_id: &str,
    out_dir: &Path,
    wasm: &Path,
) -> anyhow::Result<()> {
    let staging = wasm_bindgen_web_staging(metadata, artifact, wasm, module_id)?;
    let out_dir = workspace_output_dir(metadata, out_dir);
    std::fs::create_dir_all(&out_dir)?;
    for name in ["wasm.js", "wasm.d.ts", "wasm_bg.wasm"] {
        std::fs::copy(staging.join(name), out_dir.join(name))?;
    }
    let (wrapper, declarations, invariant) = match module_id {
        "@seekdeep-ai/seekdeep-client-ui-slots" => (
            ui_slots_esm_wrapper(),
            ui_slots_esm_declarations(),
            "client-ui-slots-invariant",
        ),
        "@seekdeep-ai/seekdeep-client-schema-form" => (
            schema_form_esm_wrapper(),
            schema_form_esm_declarations(),
            "client-schema-form-invariant",
        ),
        "@seekdeep-ai/seekdeep-client-web-react" => (
            client_web_react_esm_wrapper(),
            client_web_react_esm_declarations(),
            "client-web-react-invariant",
        ),
        "@seekdeep-ai/seekdeep-client-test-runtime" => (
            client_test_runtime_esm_wrapper(),
            client_test_runtime_esm_declarations(),
            "client-test-runtime-invariant",
        ),
        _ => anyhow::bail!("unsupported foundation ESM package {module_id}"),
    };
    let wrapper = if wrapper.contains("__SEEKDEEP_WASM_BASE64__") {
        wrapper.replace(
            "__SEEKDEEP_WASM_BASE64__",
            &base64::engine::general_purpose::STANDARD
                .encode(std::fs::read(staging.join("wasm_bg.wasm"))?),
        )
    } else {
        wrapper.to_owned()
    };
    std::fs::write(out_dir.join("index.js"), wrapper)?;
    std::fs::write(
        out_dir.join("invariant.js"),
        format!(
            "export const name = '{invariant}';\nexport const inject = ['invariants'];\nexport const apply = () => {{}};\n"
        ),
    )?;
    let type_dir = out_dir.join("types");
    if type_dir.exists() {
        std::fs::remove_dir_all(&type_dir)?;
    }
    std::fs::create_dir_all(&type_dir)?;
    std::fs::write(type_dir.join("index.d.ts"), declarations)?;
    std::fs::write(
        type_dir.join("invariant.d.ts"),
        format!(
            "export declare const name: '{invariant}';\nexport declare const inject: readonly ['invariants'];\nexport declare function apply(): void;\n"
        ),
    )?;
    println!(
        "built {module_id} Rust/WASM ESM runtime at {}",
        out_dir.join("index.js").display()
    );
    Ok(())
}

fn ui_slots_esm_wrapper() -> &'static str {
    r"import init, * as wasm from './wasm.js';

await init({ module_or_path: new URL('./wasm_bg.wasm', import.meta.url) });
export const SlotCore = wasm.SlotCore;
export const resolveSlotLabel = wasm.resolveSlotLabel;
export class StaleAuthorizationError extends Error {
  constructor(message = 'slot render authorization is stale') { super(message); this.name = 'StaleAuthorizationError'; }
}
export class SlotOwnershipError extends Error {
  constructor(message = 'slot is outside the declaring entry\'s children authorization') { super(message); this.name = 'SlotOwnershipError'; }
}
"
}

fn ui_slots_esm_declarations() -> &'static str {
    r"export type SlotKind = 'single' | 'list' | 'keyed' | 'chain';
export type SlotScope = 'root' | 'session-maybe' | 'session';
export interface SlotSpec { kind: SlotKind; scope: SlotScope; inject?: unknown }
export interface SlotRegistrationOptions { name: string; key?: string; id?: string; order?: number; priority?: number; locale?: string; registrant?: string; children?: Record<string, SlotSpec>; inject?: unknown; select?: Function; store?: unknown }
export declare class SlotCore {
  register(options: SlotRegistrationOptions, component: unknown): () => void;
  isLive(entry: unknown): boolean;
  entries(key: string): unknown[];
  entriesOfSlot(key: string): unknown[];
  spec(key: string): SlotSpec | undefined;
  specDynamic(key: string): SlotSpec | undefined;
  declarationEpoch(key: string): number;
  getVersion(key: string): number;
  subscribe(key: string, listener: () => void): () => void;
  subscribeDeclaration(key: string, listener: () => void): () => void;
  onMutate(listener: (key: string) => void): () => void;
  onEntryError(listener: Function): () => void;
  reportEntryError(key: string, entry: unknown, error: unknown, info: unknown): void;
  snapshot(root?: string): unknown;
}
export declare function resolveSlotLabel(label: string | (() => string)): string;
export declare class StaleAuthorizationError extends Error {}
export declare class SlotOwnershipError extends Error {}
export interface SlotMap {}
export interface LocaleNamespaceMap {}
export type HostObservable<T = unknown> = { getSnapshot(): T; subscribe(listener: () => void): () => void };
export type SnapshotSelectorHook<T extends object = object> = <R>(selector: (snapshot: T) => R) => R;
export type PropsRuntime<K extends string = string> = object;
export type PropsRenderSlots<K extends string = string> = object;
export type PropsLocale<N extends string = string> = { t(key: string, params?: Record<string, unknown>): string };
"
}

fn schema_form_esm_wrapper() -> &'static str {
    r"import init, * as wasm from './wasm.js';

await init({ module_or_path: new URL('./wasm_bg.wasm', import.meta.url) });
export const rehydrateSchema = wasm.rehydrateSchema;
export const validateDraft = wasm.validateDraft;
export const nodeAtPath = wasm.nodeAtPath;
export const getPath = wasm.getPath;
export const hasPath = wasm.hasPath;
export const setPath = wasm.setPath;
export const deletePath = wasm.deletePath;
"
}

fn schema_form_esm_declarations() -> &'static str {
    r"export interface SchemaNode { readonly type: string; __seekdeepValidate(draft: unknown): void; __seekdeepNodeAtPath(path: readonly string[]): SchemaNode | undefined }
export declare function rehydrateSchema(serialized: unknown): SchemaNode;
export declare function validateDraft(schema: SchemaNode, draft: unknown): string | undefined;
export declare function nodeAtPath(schema: SchemaNode, path: readonly string[]): SchemaNode | undefined;
export declare function getPath(value: unknown, path: readonly string[]): unknown;
export declare function hasPath(value: unknown, path: readonly string[]): boolean;
export declare function setPath(root: unknown, path: readonly string[], value: unknown): unknown;
export declare function deletePath(root: unknown, path: readonly string[]): unknown;
"
}

fn client_web_react_esm_wrapper() -> &'static str {
    r"import init, * as wasm from './wasm.js';
import * as React from 'react';

await init({ module_or_path: new URL('./wasm_bg.wasm', import.meta.url) });
wasm.configureClientWebReact(React, wasm.createSelectorShim(React));
const errors = wasm.webReactErrorClasses();

export const createSlotRenderer = wasm.createSlotRenderer;
export const SessionProvider = wasm.sessionProviderComponent();
export const bindSnapshotSelector = wasm.bindSnapshotSelector;
export const useInvoke = wasm.useInvoke;
export const SlotAssemblyError = errors.SlotAssemblyError;
export const StaleAuthorizationError = errors.StaleAuthorizationError;
export const SlotOwnershipError = errors.SlotOwnershipError;
"
}

fn client_web_react_esm_declarations() -> &'static str {
    r"import type { ComponentType, ReactNode } from 'react';
import type { HostObservable, SnapshotSelectorHook } from '@seekdeep-ai/seekdeep-client-ui-slots';
export declare function bindSnapshotSelector<T extends object>(source: HostObservable<T>): SnapshotSelectorHook<T>;
export declare function createSlotRenderer(): { renderRoot(): ReactNode };
export interface SessionProviderProps { children?: ReactNode; info?: unknown }
export declare const SessionProvider: ComponentType<SessionProviderProps>;
export declare function useInvoke(action: Function): readonly [boolean, (...args: unknown[]) => Promise<unknown>];
export declare class SlotAssemblyError extends Error {}
export declare class StaleAuthorizationError extends Error {}
export declare class SlotOwnershipError extends Error {}
export type UseSession<Snap extends object = object> = SnapshotSelectorHook<Snap>;
"
}

#[allow(clippy::too_many_lines)] // The self-contained ESM boundary stays reviewable as one artifact.
fn client_test_runtime_esm_wrapper() -> &'static str {
    r"import * as wasm from './wasm.js';
import * as React from 'react';
import { act, render } from '@testing-library/react';
import { within } from '@testing-library/dom';
import { afterEach, beforeEach, expect, vi } from 'vitest';
import { produce } from 'immer';

const binary = atob('__SEEKDEEP_WASM_BASE64__');
wasm.initSync({ module: Uint8Array.from(binary, value => value.charCodeAt(0)) });

const FILTER = Symbol.for('cordis.filter');
const EFFECT = Symbol.for('cordis.effect');
const ISOLATE = Symbol.for('cordis.isolate');
const INTERCEPT = Symbol.for('cordis.intercept');
const SERVICE_TRACKER = Symbol.for('cordis.service.tracker');
const INIT_HOOKS = Symbol.for('cordis.initHooks');
const INIT = Symbol.for('cordis.init');
const CHECK_PROTO = Symbol.for('cordis.checkProto');
const GeneratorFunction = function* () {}.constructor;
const AsyncGeneratorFunction = async function* () {}.constructor;
function isConstructor(value) {
  if (!value.prototype) return false;
  if (value instanceof GeneratorFunction) return false;
  if (AsyncGeneratorFunction !== Function && value instanceof AsyncGeneratorFunction) return false;
  return true;
}
function invokePlugin(plugin, ctx, config) {
  if (typeof plugin !== 'function') return plugin.apply(ctx, config);
  if (!isConstructor(plugin)) return plugin(ctx, config);
  const instance = new plugin(ctx, config);
  for (const hook of instance?.[INIT_HOOKS] ?? []) hook();
  return instance?.[INIT]?.();
}
function resolveInject(inject, result = Object.create(null)) {
  if (!inject) return Object.keys(result);
  if (Array.isArray(inject)) {
    for (const name of inject) result[name] = null;
  } else if (Reflect.has(inject, CHECK_PROTO)) {
    resolveInject(Object.getPrototypeOf(inject), result);
    for (const name of Object.keys(inject)) result[name] = inject[name] ?? null;
  } else {
    for (const name of Object.keys(inject)) result[name] = inject[name] ?? null;
  }
  return Object.keys(result);
}
function traceService(ctx, value) {
  if ((typeof value !== 'object' && typeof value !== 'function') || value === null || value[SERVICE_TRACKER] !== true) return value;
  let proxy;
  proxy = new Proxy(value, {
    get(target, key, receiver) {
      if (key === 'ctx') return ctx;
      const inner = Reflect.get(target, key, receiver);
      return typeof inner === 'function' ? (...args) => Reflect.apply(inner, proxy, args) : inner;
    },
    set(target, key, next, receiver) {
      if (key === 'ctx') return false;
      return Reflect.set(target, key, next, receiver);
    },
  });
  return proxy;
}
function wrapContext(core) {
  let context;
  context = new Proxy(core, {
    get(target, key, receiver) {
      if (key === 'emit') return (name, ...args) => target.emitArgs(name, args);
      if (key === 'parallel') return (name, ...args) => target.parallelArgs(name, args);
      if (key === 'serial') return (name, ...args) => target.serialArgs(name, args);
      if (key === 'bail') return (name, ...args) => target.bailArgs(name, args);
      if (key === 'get') return name => traceService(context, target.get(name));
      if (key === 'constructor') return wasm.WasmContext;
      if (Reflect.has(target, key)) {
        const value = Reflect.get(target, key, receiver);
        return typeof value === 'function' ? value.bind(target) : value;
      }
      const metadata = target.metaGet(key);
      if (metadata !== undefined) return metadata;
      return typeof key === 'string' ? traceService(context, target.get(key)) : undefined;
    },
    set(target, key, value, receiver) {
      if (Reflect.has(target, key) || typeof key !== 'string') return Reflect.set(target, key, value, receiver);
      return target.setProperty(key, value);
    },
    has(target, key) {
      if (Reflect.has(target, key)) return true;
      if (target.metaGet(key) !== undefined) return true;
      return typeof key === 'string' && target.get(key) !== undefined;
    },
  });
  return context;
}
Object.defineProperties(wasm.WasmContext, {
  filter: { value: FILTER },
  effect: { value: EFFECT },
  isolate: { value: ISOLATE },
  intercept: { value: INTERCEPT },
});
wasm.configureContextWrapper(wrapContext);

export const domSnapshotSerializer = {
  test(value) {
    return typeof Element !== 'undefined'
      && value instanceof Element
      && wasm.snapshotNeedsNormalization(value);
  },
  serialize(value, config, indentation, depth, refs, printer) {
    return printer(wasm.normalizeDomSnapshot(value), config, indentation, depth, refs);
  },
};

let serializerRegistered = false;
export function registerDomSnapshotSerializer() {
  if (serializerRegistered) return;
  serializerRegistered = true;
  expect.addSnapshotSerializer(domSnapshotSerializer);
}

const stabilize = async callback => {
  await act(async () => { await callback(); });
};

wasm.configureClientTestRuntime({
  createContext: () => wasm.createContext(),
  stabilize,
  act,
  produce,
  react: React,
  render,
  within,
  registerSnapshotSerializer: registerDomSnapshotSerializer,
  clearStorage: () => localStorage.clear(),
  isHtmlElement: value => typeof HTMLElement !== 'undefined' && value instanceof HTMLElement,
  invokePlugin,
  resolveInject,
});

const adopt = (face, target) => {
  Object.setPrototypeOf(face, target.prototype);
  return face;
};

export class TestRemote {
  constructor(ctx) { return adopt(wasm.installTestRemote(ctx), new.target); }
}

export class FixtureSession {
  constructor(sessionId, store, overrides = {}) {
    return adopt(wasm.createFixtureSessionFromStore(sessionId, store, overrides), new.target);
  }
}
wasm.configureFixtureSessionPrototype(FixtureSession.prototype);

export class TestSessions {
  constructor(stabilizer, rootCtx) {
    return adopt(wasm.createTestSessions(stabilizer, rootCtx, produce), new.target);
  }
}

export class TestWorkspaces {
  constructor(stabilizer) {
    return adopt(wasm.createTestWorkspaces(stabilizer, produce), new.target);
  }
}

export class TestRoot {
  constructor(slots, stabilizer) {
    return adopt(wasm.createTestRoot(slots, stabilizer), new.target);
  }
}

export class SlotTestRuntime extends wasm.SlotTestRuntime {
  static async create() {
    const runtime = await wasm.SlotTestRuntime.create();
    adopt(runtime.root, TestRoot);
    adopt(runtime.sessions, TestSessions);
    adopt(runtime.workspaces, TestWorkspaces);
    return adopt(runtime, this);
  }
}
export const conversationSnapshot = wasm.conversationSnapshot;
export const workspaceListState = wasm.workspaceListState;
export const stubSettingsScope = () => wasm.createStubSettingsScope(implementation => vi.fn(implementation));
export const makeTranslate = (...dictionaries) => wasm.makeTranslate(Array.from(dictionaries));

export function usePinnedBrowserLanguages(primary, ...rest) {
  let pin;
  beforeEach(() => { pin = new wasm.WasmBrowserLanguagePin(primary, Array.from(rest)); });
  afterEach(() => { pin?.dispose(); pin = undefined; });
}
"
}

#[allow(clippy::too_many_lines)] // The source-compatible declaration surface is one closed artifact.
fn client_test_runtime_esm_declarations() -> &'static str {
    r"import type { Context, Fiber, Plugin } from '@seekdeep-ai/cordis';
import type { BoundFunctions, queries } from '@testing-library/dom';
import type { RenderResult } from '@testing-library/react';
import type { ReactNode } from 'react';
import type { Mock, SnapshotSerializer } from 'vitest';
import type {
  SessionId,
  SessionListState,
  SessionSummary,
  SettingsScope,
  SettingsScopeSnapshot,
  WorkspaceId,
} from '@seekdeep-ai/seekdeep-client-runtime/client';
import type { SlotMap } from '@seekdeep-ai/seekdeep-client-ui-slots';

export type Stabilizer = (callback: () => void | Promise<void>) => Promise<void>;
export interface ObservableSnapshot<T> {
  getSnapshot(): T;
  subscribe(listener: () => void): () => void;
}
export interface ConversationSnapshot extends Record<string, unknown> {
  sessionId: SessionId;
  running: boolean;
}
export interface SessionBehaviorOverrides extends Record<string, unknown> {
  prompt?: Function;
  readAttachment?: Function;
  updateQueue?: Function;
  cancel?: Function;
  command?: Function;
  loadOlder?: Function;
  rename?: Function;
}
export interface SessionFixture {
  id: string;
  snapshot?: Partial<Omit<ConversationSnapshot, 'sessionId'>>;
  summary?: Partial<Omit<SessionSummary, 'id'>>;
  session?: SessionBehaviorOverrides;
}
export interface SessionProvideInfo {
  sessionId: SessionId;
  hooks: Record<string, ObservableSnapshot<unknown>>;
  props: Record<string, unknown>;
  projections?: { faceOf(key: string): ObservableSnapshot<unknown> };
}
export interface SessionMaybeProvideInfo {
  sessionId: SessionId | undefined;
  hooks: Record<string, ObservableSnapshot<unknown> | undefined>;
  props: Record<string, unknown | undefined>;
  projections?: { faceOf(key: string): ObservableSnapshot<unknown> };
}
export interface SessionProvideDescriptor {
  hooks?: readonly string[];
  props?: readonly string[];
  resolve(binding: TestSessionBinding): {
    hooks?: Record<string, ObservableSnapshot<unknown>>;
    props?: Record<string, unknown>;
  };
}
export interface SessionSearchResultItem extends Record<string, unknown> {
  sessionId: SessionId;
  snippet: string;
}
export interface SubagentAddress extends Record<string, unknown> {
  parentSessionId: SessionId;
  childSessionId: SessionId;
}
export interface WorkspaceView extends Record<string, unknown> {
  workspaceId: WorkspaceId;
  title: string;
  path: string;
  sessionIds: SessionId[];
}
export interface WorkspaceListState extends Record<string, unknown> {
  items: WorkspaceView[];
  archivedSessionIds: SessionId[];
  state: string;
  phase: string;
  error: unknown;
  baselinesReady: boolean;
  recentWorkspaceId?: WorkspaceId;
}
export interface DirectoryListing extends Record<string, unknown> {
  path: string;
  home: string;
  crumbs: unknown[];
  entries: unknown[];
}
export interface StubSettingsScope<T> {
  scope: SettingsScope<T>;
  set: Mock;
  unset: Mock;
  listenerCount(): number;
  publish(next: Partial<SettingsScopeSnapshot<T>>): void;
}
export declare class TestRemote {
  constructor(ctx: Context);
  $dispatch(event: string, args: readonly unknown[]): void;
  $on(event: string, listener: (...args: never[]) => void): () => void;
  $mount(): Promise<() => Promise<void>>;
}
export declare class FixtureSession implements ObservableSnapshot<ConversationSnapshot> {
  constructor(sessionId: SessionId, store: ObservableSnapshot<ConversationSnapshot>, overrides: SessionBehaviorOverrides);
  readonly sessionId: SessionId;
  readonly projections: {
    faceOf(key: string): ObservableSnapshot<unknown>;
    set(key: string, value: unknown): void;
  };
  getSnapshot(): ConversationSnapshot;
  subscribe(listener: () => void): () => void;
  prompt(...args: unknown[]): never;
  readAttachment(...args: unknown[]): never;
  updateQueue(...args: unknown[]): never;
  cancel(...args: unknown[]): never;
  command(...args: unknown[]): never;
  loadOlder(...args: unknown[]): never;
  rename(...args: unknown[]): never;
}
export interface TestSessionBinding {
  readonly sessionId: SessionId;
  readonly session: FixtureSession;
  readonly ctx: Context;
}
export declare class TestSessions {
  constructor(stabilizer: Stabilizer, rootCtx: Context);
  readonly list: ObservableSnapshot<SessionListState>;
  readonly currentProvideInfo: ObservableSnapshot<SessionMaybeProvideInfo>;
  readonly calls: {
    method: 'open' | 'openSubagent' | 'setSubagentCatalogOpen' | 'refreshSubagents' | 'clear' | 'search' | 'fork';
    args: unknown[];
  }[];
  readonly searchResultLimit: number;
  add(fixture: SessionFixture, options?: { current?: boolean }): Promise<SessionId>;
  updateSnapshot(id: string, mutate: (draft: ConversationSnapshot) => void): Promise<void>;
  updateSummary(id: string, patch: Partial<Omit<SessionSummary, 'id'>>): Promise<void>;
  setCurrent(id: string | undefined): Promise<void>;
  remove(id: string): Promise<void>;
  provide(descriptor: SessionProvideDescriptor): () => void;
  provideInfo(id: string): SessionProvideInfo | undefined;
  maybeProvideInfo(id: string | undefined): SessionMaybeProvideInfo;
  scope(id: string): Context | undefined;
  binding(id: string): TestSessionBinding | undefined;
  scopeOf(ctx: Context): SessionId | undefined;
  sessionOf(ctx: Context): FixtureSession | undefined;
  open(id: SessionId): void;
  openSubagent(address: SubagentAddress): void;
  subagentAddress(id: SessionId): SubagentAddress | undefined;
  setSubagentCatalogOpen(parentSessionId: SessionId, open: boolean): void;
  refreshSubagents(parentSessionId: SessionId): Promise<void>;
  noteAgentPreset(sessionId: SessionId, agentPreset: string): void;
  clear(): void;
  stubSearch(implementation: (query: string, signal: AbortSignal) => { items: SessionSearchResultItem[]; hasMore: boolean }): void;
  search(query: string, signal: AbortSignal): Promise<{ ok: true; value: { items: SessionSearchResultItem[]; hasMore: boolean } }>;
  fork(options: { sessionId: SessionId; atSeq?: number; increaseTitle?: boolean }): Promise<SessionId>;
  behavior(id: string): FixtureSession;
  disposeScopes(): Promise<void>;
}
export declare class TestWorkspaces {
  constructor(stabilizer: Stabilizer);
  readonly list: ObservableSnapshot<WorkspaceListState>;
  readonly calls: { method: string; args: unknown[] }[];
  stub(method: string, implementation: (...args: unknown[]) => unknown): void;
  update(mutator: (draft: WorkspaceListState) => void): Promise<void>;
  connectWorkspace(workspaceId: WorkspaceId): Promise<SessionId>;
  startSession(workspaceId?: WorkspaceId): void;
  create(input: { path: string }): Promise<WorkspaceView>;
  openPath(path: string): Promise<void>;
  pickDirectory(): Promise<string | null>;
  listDirectory(path?: string, signal?: AbortSignal): Promise<DirectoryListing>;
  createDirectory(path: string, name: string): Promise<string>;
  rename(workspaceId: WorkspaceId, title: string): Promise<WorkspaceView>;
  delete(workspaceId: WorkspaceId): Promise<void>;
  insertBefore(workspaceId: WorkspaceId, beforeWorkspaceId?: WorkspaceId): Promise<void>;
  insertSessionBefore(workspaceId: WorkspaceId, sessionId: SessionId, beforeSessionId?: SessionId): Promise<WorkspaceView>;
  archiveSession(sessionId: SessionId): Promise<void>;
}
export type SlotKind = 'single' | 'list' | 'keyed' | 'chain';
export type SlotScope = 'root' | 'session-maybe' | 'session';
export interface ChildSlotDeclaration {
  kind: SlotKind;
  scope: SlotScope;
  inject?: unknown;
}
export type ChildrenDecl = Record<string, ChildSlotDeclaration>;
export type SlotKey = keyof SlotMap & string;
export type OwnerOf<K extends SlotKey> = SlotMap[K] extends { owner: infer Owner } ? Owner : object;
export type SlotComponent<Props extends object = object> = (props: Props) => ReactNode;
export interface ComposedProps<Children extends SlotKey = never> {
  renderSlot<K extends Children>(key: K, owner: OwnerOf<K>, options?: Record<string, unknown>): ReactNode;
  renderSlotChain<K extends Children>(key: K, owner: OwnerOf<K>, options?: Record<string, unknown>): ReactNode;
  SessionProvider: SlotComponent<Record<string, unknown>>;
}
export interface StoreInstanceLike<State = unknown> extends ObservableSnapshot<State> {
  readonly actions: Record<string, Function>;
  clearPersisted?(): void;
}
export interface SlotRegistry {
  register(options: { name: string; [key: string]: unknown }, component: unknown): () => void;
  entries(key: string): unknown[];
  spec(key: string): ChildSlotDeclaration | undefined;
  renderSlot(key: string, owner: object): ReactNode;
  pruneStoreScope(sessionId: string): void;
}
export declare class TestRoot {
  constructor(slots: SlotRegistry, stabilizer: Stabilizer);
  declare<const D extends ChildrenDecl>(
    children: D,
    frame: SlotComponent<ComposedProps<keyof NoInfer<D> & SlotKey>>,
  ): Promise<void>;
  release(): void;
}
export interface FeatureHandle {
  readonly fiber: Fiber;
  dispose(): Promise<void>;
}
export interface SlotView<K extends SlotKey> {
  readonly container: HTMLElement;
  readonly view: BoundFunctions<typeof queries>;
  update(owner: OwnerOf<K>): void;
}
export declare class SlotTestRuntime {
  static create(): Promise<SlotTestRuntime>;
  readonly ctx: Context;
  readonly slots: SlotRegistry;
  readonly root: TestRoot;
  readonly sessions: TestSessions;
  readonly workspaces: TestWorkspaces;
  provide<K extends string>(name: K, value: K extends keyof Context ? Partial<Context[K]> : unknown): void;
  mount(plugin: Plugin): Promise<FeatureHandle>;
  renderRoot(): RenderResult;
  declare(children: ChildrenDecl): Promise<void>;
  renderSlot<K extends SlotKey>(key: K, owner: OwnerOf<K>): SlotView<K>;
  storeOf(key: SlotKey, scopeKey?: string): StoreInstanceLike;
  flush(): Promise<void>;
  dispose(): Promise<void>;
}
export declare const domSnapshotSerializer: SnapshotSerializer;
export declare function registerDomSnapshotSerializer(): void;
export declare function conversationSnapshot(sessionId: SessionId): ConversationSnapshot;
export declare function workspaceListState(): WorkspaceListState;
export declare function stubSettingsScope<T>(): StubSettingsScope<T>;
export declare function makeTranslate(...dictionaries: readonly Record<string, string>[]): (key: string, params?: Record<string, unknown>) => string;
export declare function usePinnedBrowserLanguages(primary: string, ...rest: string[]): void;
"
}

fn wasm_bindgen_web_staging(
    metadata: &CargoMetadata,
    artifact: &str,
    wasm: &Path,
    label: &str,
) -> anyhow::Result<PathBuf> {
    let staging = metadata
        .target_directory
        .join("xtask/wasm-package")
        .join(artifact);
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    std::fs::create_dir_all(&staging)?;
    let status = ProcessCommand::new("wasm-bindgen")
        .args(["--target", "web", "--out-name", "wasm", "--out-dir"])
        .arg(&staging)
        .arg(wasm)
        .status()?;
    anyhow::ensure!(status.success(), "wasm-bindgen failed for {label}");
    Ok(staging)
}

fn workspace_output_dir(metadata: &CargoMetadata, out_dir: &Path) -> PathBuf {
    if out_dir.is_absolute() {
        out_dir.to_owned()
    } else {
        metadata.workspace_root.join(out_dir)
    }
}

fn client_modules_esm_wrapper() -> &'static str {
    r"import init, * as wasm from './wasm.js';

await init({ module_or_path: new URL('./wasm_bg.wasm', import.meta.url) });
const plugin = wasm.clientModulesPlugin();

export class ClientModuleSystem {
  constructor(options) {
    return new wasm.WasmClientModuleSystem(
      options.modules,
      options.staticModules,
      options.loadBundle,
    );
  }
}
export const parseBootManifest = wasm.parseBootManifest;
export const name = plugin.name;
export const inject = Object.freeze([]);
export const apply = plugin.apply;
"
}

fn client_modules_declarations() -> &'static str {
    r"import type { Context } from '@seekdeep-ai/cordis';
export interface BootModuleRow { id: string; url: string; rev: string }
export interface WebBootEntry extends BootModuleRow { inject: string[]; immediately: boolean }
export interface BootManifest { rev: string; modules: BootModuleRow[]; plugins: WebBootEntry[] }
export interface ClientModuleSystemOptions { modules: BootModuleRow[]; staticModules: Record<string, unknown>; loadBundle?: (url: string) => Promise<void> }
export declare class ClientModuleSystem {
  constructor(options: ClientModuleSystemOptions);
  readonly version: 'client';
  readonly loadCache: Map<string, unknown>;
  import(id: string): Promise<unknown>;
  registerStatic(id: string, value: unknown): void;
  prefetch(id: string): Promise<void>;
  invalidate(id: string): void;
}
export declare function parseBootManifest(value: unknown): BootManifest;
export declare const name: 'client-modules';
export declare const inject: readonly [];
export declare function apply(context: Context): void;
"
}

fn client_loader_esm_wrapper() -> &'static str {
    r"import init, * as wasm from './client.js';

await init({ module_or_path: new URL('./client_bg.wasm', import.meta.url) });
const plugin = wasm.clientLoaderPlugin();

export const Loader = wasm.WasmClientLoader;
export const name = plugin.name;
export const inject = Object.freeze([]);
export const apply = plugin.apply;
export default plugin;
"
}

fn client_loader_esm_declarations() -> &'static str {
    r"import type { Context, Disposable, PluginObject } from '@seekdeep-ai/cordis';
export interface EntryOptions { id?: string; name: string; config?: unknown; group?: boolean | null; disabled?: boolean | null; inject?: readonly string[] | Record<string, unknown> | null }
export interface Entry { options: Required<Pick<EntryOptions, 'id' | 'name'>> & EntryOptions; fiber?: { state: number; inject: Record<string, unknown>; await(): Promise<void>; dispose(): Promise<void> } }
export declare class Loader {
  constructor(context: Context);
  internal: { import(name: string, parentUrl?: string, attributes?: object): Promise<unknown> };
  create(options: EntryOptions): Promise<string>;
  resolve(id: string): Entry;
  entries(): Entry[];
  await(): Promise<void>;
  remove(id: string): Promise<void>;
}
export declare const name: 'loader';
export declare const inject: readonly [];
export declare const apply: (context: Context) => void;
declare const plugin: PluginObject;
export default plugin;
"
}

fn cordis_esm_wrapper() -> &'static str {
    r"import init, * as wasm from './client.js';

await init({ module_or_path: new URL('./client_bg.wasm', import.meta.url) });

const FILTER = Symbol.for('cordis.filter');
const EFFECT = Symbol.for('cordis.effect');
const ISOLATE = Symbol.for('cordis.isolate');
const INTERCEPT = Symbol.for('cordis.intercept');
const SERVICE_TRACKER = Symbol.for('cordis.service.tracker');

function traceService(ctx, value) {
  if ((typeof value !== 'object' && typeof value !== 'function') || value === null || value[SERVICE_TRACKER] !== true) return value;
  let proxy;
  proxy = new Proxy(value, {
    get(target, key, receiver) {
      if (key === 'ctx') return ctx;
      const inner = Reflect.get(target, key, receiver);
      return typeof inner === 'function'
        ? (...args) => Reflect.apply(inner, proxy, args)
        : inner;
    },
    set(target, key, next, receiver) {
      if (key === 'ctx') return false;
      return Reflect.set(target, key, next, receiver);
    },
  });
  return proxy;
}

function wrapContext(core) {
  let context;
  context = new Proxy(core, {
    get(target, key, receiver) {
      if (key === 'emit') return (name, ...args) => target.emitArgs(name, args);
      if (key === 'parallel') return (name, ...args) => target.parallelArgs(name, args);
      if (key === 'serial') return (name, ...args) => target.serialArgs(name, args);
      if (key === 'bail') return (name, ...args) => target.bailArgs(name, args);
      if (key === 'get') return name => traceService(context, target.get(name));
      if (Reflect.has(target, key)) {
        const value = Reflect.get(target, key, receiver);
        return typeof value === 'function' ? value.bind(target) : value;
      }
      const metadata = target.metaGet(key);
      if (metadata !== undefined) return metadata;
      return typeof key === 'string' ? traceService(context, target.get(key)) : undefined;
    },
    set(target, key, value, receiver) {
      if (Reflect.has(target, key) || typeof key !== 'string') {
        return Reflect.set(target, key, value, receiver);
      }
      return target.setProperty(key, value);
    },
    has(target, key) {
      if (Reflect.has(target, key)) return true;
      if (target.metaGet(key) !== undefined) return true;
      return typeof key === 'string' && target.get(key) !== undefined;
    },
  });
  return context;
}

Object.defineProperties(wasm.WasmContext, {
  filter: { value: FILTER },
  effect: { value: EFFECT },
  isolate: { value: ISOLATE },
  intercept: { value: INTERCEPT },
});
wasm.configureContextWrapper(wrapContext);

export class Context {
  static filter = FILTER;
  static effect = EFFECT;
  static isolate = ISOLATE;
  static intercept = INTERCEPT;
  static is(value) { return value?.__seekdeepContext instanceof wasm.WasmContext; }
  constructor() { return wasm.createContext(); }
}

export const Fiber = wasm.WasmFiber;
export const FiberState = Object.freeze({ PENDING: 0, LOADING: 1, ACTIVE: 2, FAILED: 3, DISPOSED: 4, UNLOADING: 5 });
export const symbols = Object.freeze({ filter: FILTER, effect: EFFECT, isolate: ISOLATE, intercept: INTERCEPT });
export class Service {
  static config = Symbol.for('cordis.service.config');
  static tracker = Symbol.for('cordis.service.tracker');
  static check = Symbol.for('cordis.service.check');
  static init = Symbol.for('cordis.service.init');
  constructor(ctx, name) {
    this.ctx = ctx;
    this.name = name;
    Object.defineProperty(this, SERVICE_TRACKER, { value: true });
    ctx.provide(name, this);
  }
}
export class CordisError extends Error {
  constructor(code, message) { super(message ?? code); this.code = code; }
}
export function Inject() { return value => value; }
"
}

fn cordis_esm_declarations() -> &'static str {
    r"export type Awaitable<T> = T | PromiseLike<T>;
export type Disposable = () => Awaitable<void>;
export type Inject = readonly string[] | Readonly<Record<string, unknown>>;
export interface PluginObject<T = unknown> { name?: string; inject?: Inject; apply(ctx: Context, config: T): unknown }
export type Plugin<T = unknown> = PluginObject<T> | ((ctx: Context, config: T) => unknown);
export interface EventOptions { prepend?: boolean; global?: boolean; once?: boolean }
export declare class Fiber {
  readonly ctx: Context;
  readonly state: number;
  readonly uid: number | null;
  readonly inject: Record<string, unknown>;
  entry?: unknown;
  await(): Promise<void>;
  then(fulfilled: Function, rejected: Function): Promise<unknown>;
  dispose(): Promise<void>;
  update(config: unknown): Promise<void>;
}
export declare class Context {
  static readonly filter: symbol;
  static readonly effect: symbol;
  static readonly isolate: symbol;
  static readonly intercept: symbol;
  static is(value: unknown): value is Context;
  readonly root: Context;
  readonly fiber: Fiber;
  readonly reflect: { provide(name: string, value: unknown, check?: unknown): Disposable };
  readonly registry: { plugin(plugin: Plugin, config?: unknown): Fiber };
  readonly events: Context;
  constructor();
  get(name: string): unknown;
  provide(name: string, value: unknown): Disposable;
  plugin(plugin: Plugin, config?: unknown): Fiber;
  inject(dependencies: Inject, callback: Plugin): Fiber;
  on(name: string, listener: Function, options?: EventOptions): Disposable;
  emit(name: string, ...args: unknown[]): void;
  parallel(name: string, ...args: unknown[]): Promise<void>;
  serial(name: string, ...args: unknown[]): Promise<unknown>;
  bail(name: string, ...args: unknown[]): unknown;
  effect(setup: () => unknown, label?: string): Disposable;
  extend(metadata?: object): Context;
  isolate(name: string, label?: string): Context;
  intercept(name: string, config: unknown): Context;
  mixin(source: string, members: readonly string[]): Disposable;
}
export declare const FiberState: Readonly<{ PENDING: 0; LOADING: 1; ACTIVE: 2; FAILED: 3; DISPOSED: 4; UNLOADING: 5 }>;
export declare const symbols: Readonly<{ filter: symbol; effect: symbol; isolate: symbol; intercept: symbol }>;
export declare class Service {
  static readonly config: symbol;
  static readonly tracker: symbol;
  static readonly check: symbol;
  static readonly init: symbol;
  readonly ctx: Context;
  readonly name: string;
  constructor(ctx: Context, name: string);
}
export declare class CordisError extends Error { readonly code: string; constructor(code: string, message?: string) }
export declare function Inject(name?: string, config?: unknown): <T>(value: T) => T;
"
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
import * as Immer from 'immer';
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

await init({ module_or_path: new URL('./client_bg.wasm', import.meta.url) });
const staticModules = {
  'react': React,
  'react/jsx-runtime': ReactJsxRuntime,
  'react-dom': ReactDom,
  'react-dom/client': ReactDomClient,
  'immer': Immer,
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

fn wasm_ui_attachment_package(
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
        "wasm-bindgen failed for client UI attachment"
    );
    let out_dir = if out_dir.is_absolute() {
        out_dir.to_owned()
    } else {
        metadata.workspace_root.join(out_dir)
    };
    std::fs::create_dir_all(&out_dir)?;
    for name in ["client.js", "client_bg.wasm"] {
        std::fs::copy(staging.join(name), out_dir.join(name))?;
    }
    std::fs::write(out_dir.join("index.js"), ui_attachment_esm_wrapper())?;
    std::fs::write(
        out_dir.join("invariant.js"),
        ui_attachment_invariant_wrapper(),
    )?;
    let type_dir = out_dir.join("types");
    if type_dir.exists() {
        std::fs::remove_dir_all(&type_dir)?;
    }
    let client_types = type_dir.join("client");
    std::fs::create_dir_all(&client_types)?;
    std::fs::copy(staging.join("client.d.ts"), client_types.join("index.d.ts"))?;
    copy_ui_attachment_type_declarations(&metadata.workspace_root, &type_dir)?;
    println!(
        "built @seekdeep-ai/seekdeep-client-ui-attachment Rust/WASM ESM library at {}",
        out_dir.join("index.js").display()
    );
    Ok(())
}

fn copy_ui_attachment_type_declarations(
    workspace: &Path,
    destination: &Path,
) -> anyhow::Result<()> {
    let source = workspace.join("packages/client/ui-attachment/assets/types");
    let mut count = 0_usize;
    for entry in walkdir::WalkDir::new(&source) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        if !name.ends_with(".d.ts.txt") {
            anyhow::ensure!(
                matches!(
                    name.as_ref(),
                    "README.md" | "README.zh.md" | "README.i18n.yaml"
                ),
                "unexpected attachment type asset: {}",
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
        count == 6,
        "expected 6 ui-attachment declarations, found {count}"
    );
    Ok(())
}

fn ui_attachment_esm_wrapper() -> &'static str {
    r"import init, * as wasm from './client.js';
import * as React from 'react';
import * as ReactDOM from 'react-dom';
import * as UiPrimitives from '@seekdeep-ai/seekdeep-client-ui-primitives';

await init({ module_or_path: new URL('./client_bg.wasm', import.meta.url) });
wasm.configureClientUiAttachment(React, ReactDOM, UiPrimitives);

export const AttachmentRail = wasm.attachmentRailComponent();
export const DropOverlay = wasm.dropOverlayComponent();
export const ImageLightbox = wasm.imageLightboxComponent();
export const MessageImage = wasm.messageImageComponent();
export const ImageGallery = wasm.imageGalleryComponent();
"
}

fn ui_attachment_invariant_wrapper() -> &'static str {
    r"const PACKAGE_NAME = '@seekdeep-ai/seekdeep-client-ui-attachment';
export const name = 'client-ui-attachment-invariant';
export const inject = ['invariants'];
const install = () => {};
export const apply = ctx => Promise.resolve(ctx.invariants.register(PACKAGE_NAME, install));
"
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
                matches!(name, "README.md" | "README.zh.md" | "README.i18n.yaml"),
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
    for name in [
        "katex.min.css",
        "LICENSE",
        "README.md",
        "README.zh.md",
        "README.i18n.yaml",
    ] {
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
    r"import katex from 'katex';
import { normalizeUri } from 'micromark-util-sanitize-uri';

export function createMarkdownBackend(cssUrl) {
  return {
    cssUrl,
    normalizeUri,
    renderTex(value, options) { return katex.renderToString(value, options); },
  };
}
"
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
        writeln!(
            wrapper,
            "export const {name} = iconComponents.{name};",
            name = definition.name
        )
        .expect("writing to String cannot fail");
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
        "@seekdeep-ai/seekdeep-client-ui-agent-preset" => "client-ui-agent-preset-invariant",
        "@seekdeep-ai/seekdeep-client-ui-settings-plugins" => {
            "client-ui-settings-plugins-invariant"
        }
        "@seekdeep-ai/seekdeep-client-ui-skill" => "client-ui-skill-invariant",
        "@seekdeep-ai/seekdeep-client-ui-subagent" => "client-ui-subagent-invariant",
        "@seekdeep-ai/seekdeep-client-ui-permission-presets" => {
            "client-ui-permission-presets-invariant"
        }
        "@seekdeep-ai/seekdeep-client-ui-model-selection" => "client-ui-model-selection-invariant",
        "@seekdeep-ai/seekdeep-client-ui-input-trigger" => "client-ui-input-trigger-invariant",
        "@seekdeep-ai/seekdeep-client-ui-commands" => "client-ui-commands-invariant",
        "@seekdeep-ai/seekdeep-client-ui-workspace" => "client-ui-workspace-invariant",
        "@seekdeep-ai/seekdeep-client-ui-directory-picker-browse" => {
            "client-ui-directory-picker-browse-invariant"
        }
        "@seekdeep-ai/seekdeep-session-log-export" => "session-log-export-invariant",
        "@seekdeep-ai/seekdeep-client-ui-conversation" => "client-ui-conversation-invariant",
        "@seekdeep-ai/seekdeep-client-ui-tool" => "client-ui-tool-invariant",
        "@seekdeep-ai/seekdeep-client-ui-settings-models" => "client-ui-settings-models-invariant",
        _ => return Ok(()),
    };
    std::fs::write(
        out_dir.join("index.js"),
        if module_id == "@seekdeep-ai/seekdeep-session-log-export" {
            r"export const name = 'session-log-download';
export const inject = ['commands'];
export function apply(ctx) {
  ctx.effect(() => ctx.commands.register({
    name: 'export',
    description: 'Download this Session log as a ZIP archive',
    handler: invocation => Promise.resolve(invocation.rawInput.trim() === ''
      ? { kind: 'success', text: 'Session log download requested.' }
      : { kind: 'error', text: 'The Web /export command does not accept a path.' }),
  }), 'session-log-download: command');
}
"
        } else {
            "export function apply() {}\n"
        },
    )?;
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
        if module_id == "@seekdeep-ai/seekdeep-session-log-export" {
            "export declare const name: 'session-log-download';\nexport declare const inject: readonly ['commands'];\nexport declare function apply(ctx: unknown): void;\n"
        } else {
            "export declare function apply(): void;\n"
        },
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

fn wasm_package_global(artifact: &str, module_id: &str) -> String {
    let artifact = artifact.replace('-', "_");
    let module = module_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("__{artifact}_{module}_wasm")
}

fn compatibility_prelude(global: &str, module_id: &str) -> String {
    if module_id == "@seekdeep-ai/seekdeep-api-remotes" {
        return format!("  {global}.configureApiRemotesZod(__seekdeepRemoteZod);\n");
    }
    if module_id == "@seekdeep-ai/seekdeep-client-locale" {
        return format!(
            "  Object.assign({global}, {{ apply: {global}.applyClientLocale, inject: ['slots', 'connection', 'remote', 'settingsScope'] }});\n"
        );
    }
    if module_id != "@seekdeep-ai/seekdeep-client-runtime" {
        return String::new();
    }
    format!(
        "  class SessionCreateError extends Error {{ constructor(rpcError, requestedSessionId) {{ super(`session create failed: ${{rpcError.code}}: ${{rpcError.message}}`); this.name = 'SessionCreateError'; this.rpcError = rpcError; this.requestedSessionId = requestedSessionId; }} }}\n  class SessionForkError extends Error {{ constructor(rpcError, sourceSessionId) {{ super(`session fork failed: ${{rpcError.code}}: ${{rpcError.message}}`); this.name = 'SessionForkError'; this.rpcError = rpcError; this.sourceSessionId = sourceSessionId; }} }}\n  class WorkspaceCreateError extends Error {{ constructor(rpcError) {{ super(`workspace create failed: ${{rpcError.code}}: ${{rpcError.message}}`); this.name = 'WorkspaceCreateError'; this.rpcError = rpcError; }} }}\n  class DirectoryBrowseError extends Error {{ constructor(rpcError) {{ super(`directory browse failed: ${{rpcError.code}}: ${{rpcError.message}}`); this.name = 'DirectoryBrowseError'; this.rpcError = rpcError; }} }}\n  const apply = ctx => {{ {global}.applyClientRuntime(ctx); }};\n  Object.assign({global}, {{ apply, inject: ['connection', 'typert', 'remote', 'remote.commands'], SlotRegistry: {global}.ClientSlotRegistry, SessionCreateError, SessionForkError, WorkspaceCreateError, DirectoryBrowseError, EMPTY_CHAT_SNAPSHOT: {global}.emptyChatSnapshot(), EMPTY_CONVERSATION_VIEWS: {global}.emptyConversationViews() }});\n"
    )
}

#[allow(clippy::too_many_lines)] // Closed module-specific factory dispatch stays auditable here.
fn module_factory(global: &str, module_id: &str) -> String {
    if module_id == "@seekdeep-ai/seekdeep-client-connection" {
        return format!("() => {global}.clientConnectionPlugin()");
    }
    if module_id == "@seekdeep-ai/seekdeep-typert-registry" {
        return format!("() => {global}.clientTypertRegistryPlugin()");
    }
    if module_id == "@seekdeep-ai/seekdeep-api-gateway" {
        return format!(
            r"() => {{
  const tracker = Symbol.for('cordis.service.tracker');
  const remoteFactory = (ctx, core) => {{
    const service = {{
      ctx,
      $mount(contribution) {{ return core.mount(this.ctx, contribution); }},
      $on(event, listener) {{ return core.on(this.ctx, event, listener); }},
      $dispatch(event, args) {{ return core.dispatch(event, args); }},
    }};
    Object.defineProperty(service, tracker, {{ value: true }});
    ctx.provide('remote', service);
    return service;
  }};
  const namespaceFactory = (ctx, namespace, invoke, install) => {{
    const service = {{
      ctx,
      namespace,
      installDirect(method, fresh) {{ return install(this, method, fresh); }},
      installScoped(method, fresh) {{ return install(this, method, fresh); }},
      install(method) {{
        Object.defineProperty(this, method, {{
          configurable: true,
          enumerable: true,
          get: function () {{ const bound = invoke(this.ctx, method); return (...args) => bound(args); }},
        }});
      }},
      remove(method) {{ delete this[method]; }},
    }};
    Object.defineProperty(service, tracker, {{ value: true }});
    Object.defineProperty(service, 'invokeRemote', {{ value: invoke }});
    const dispose = ctx.provide('remote.' + namespace, service);
    return {{ service, dispose }};
  }};
  {global}.configureClientApiGateway(remoteFactory, namespaceFactory);
  return {global}.clientApiGatewayPlugin();
}}"
        );
    }
    if module_id == "@seekdeep-ai/seekdeep-client-runtime" {
        return format!(
            "require => {{ {global}.installStoreProduce(require('immer').produce); return {global}; }}"
        );
    }
    if module_id == "@seekdeep-ai/seekdeep-client-hmr" {
        return format!("() => {global}.clientHmrPlugin()");
    }
    if module_id == "@seekdeep-ai/seekdeep-cordis-client-runner" {
        return format!("require => {global}.cordisClientRunnerPlugin(require('react'))");
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-cordis" {
        return format!(
            "require => {global}.clientUiCordisPlugin(require('react'), require('@seekdeep-ai/seekdeep-client-ui-primitives'))"
        );
    }
    if module_id == "@seekdeep-ai/seekdeep-api-remotes" {
        return format!(
            "() => {{ {global}.configureApiRemotes({global}.generatedApiRemotes()); return {{ name: 'api-remotes', apply: {global}.applyApiRemotes, inject: ['remote'] }}; }}"
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
    if module_id == "@seekdeep-ai/seekdeep-client-ui-agent-preset" {
        return ui_agent_preset_module_factory(global);
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-settings-plugins" {
        return ui_settings_plugins_module_factory(global);
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
    if module_id == "@seekdeep-ai/seekdeep-client-ui-workspace" {
        return ui_workspace_module_factory(global);
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-directory-picker-browse" {
        return ui_directory_picker_browse_module_factory(global);
    }
    if module_id == "@seekdeep-ai/seekdeep-session-log-export" {
        return session_log_export_module_factory(global);
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-conversation" {
        return ui_conversation_module_factory(global);
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-tool" {
        return ui_tool_module_factory(global);
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-settings-models" {
        return ui_settings_models_module_factory(global);
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

fn ui_workspace_module_factory(global: &str) -> String {
    format!(
        "require => {{ const runtime = require('@seekdeep-ai/seekdeep-client-runtime/client'); {global}.configureClientUiWorkspace(require('react'), require('@seekdeep-ai/seekdeep-client-ui-primitives')); {global}.configureClientUiWorkspaceApply(runtime.defineStore); Object.assign({global}, {{ apply: {global}.applyClientUiWorkspace, inject: ['slots', 'sessions', 'workspaces', 'locale'], FLAT_SESSION_ORDER_KEY: {global}.flatSessionOrderKey() }}); return {global}; }}"
    )
}

fn ui_directory_picker_browse_module_factory(global: &str) -> String {
    format!(
        "require => {{ {global}.configureClientUiDirectoryPickerBrowse(require('react'), require('@seekdeep-ai/seekdeep-client-ui-primitives')); Object.assign({global}, {{ apply: {global}.applyClientUiDirectoryPickerBrowse, inject: ['slots', 'workspaces', 'locale'] }}); return {global}; }}"
    )
}

fn session_log_export_module_factory(global: &str) -> String {
    r"require => {
  const g = __GLOBAL__;
  g.configureSessionLogExport(require('react'), require('@seekdeep-ai/seekdeep-client-ui-primitives'));
  class SessionLogDownloadController {
    constructor(fetcher, saver) {
      const face = g.createSessionLogDownloadController(fetcher, saver);
      Object.setPrototypeOf(face, new.target.prototype);
      return face;
    }
  }
  g.configureSessionLogExportApply(SessionLogDownloadController);
  const downloadUrl = (url, filename) => {
    const anchor = document.createElement('a');
    anchor.href = url;
    anchor.download = filename;
    anchor.click();
  };
  Object.assign(g, {
    apply: g.applySessionLogExport,
    inject: ['slots', 'locale'],
    SessionLogDownloadController,
    downloadUrl,
  });
  return g;
}"
        .replace("__GLOBAL__", global)
}

fn ui_conversation_module_factory(global: &str) -> String {
    r"require => {
  const g = __GLOBAL__;
  const React = require('react');
  const primitives = require('@seekdeep-ai/seekdeep-client-ui-primitives');
  const attachment = require('@seekdeep-ai/seekdeep-client-ui-attachment');
  const runtime = require('@seekdeep-ai/seekdeep-client-runtime/client');
  g.configureClientUiConversationReasoning(React, primitives);
  g.configureClientUiConversationMessageActions(React, primitives);
  g.configureClientUiConversationCommand(React, primitives);
  g.configureClientUiConversationContextBodies(React, primitives);
  g.configureClientUiConversationMessageItem(React, primitives, attachment, {
    MessageIconActions: g.messageIconActionsComponent(),
    CompactionItem: g.compactionItemComponent(),
    ContextInjectionRow: g.contextInjectionRowComponent(),
  });
  g.configureClientUiConversationAssistant(React, primitives, attachment);
  g.configureClientUiConversationChatSeat(React, primitives);
  g.configureClientUiConversationTurnTail(React);
  g.configureClientUiConversationChatView(React, primitives, {
    ChatNodeSeat: g.chatNodeSeatComponent(),
    PendingSteeringBubble: g.pendingSteeringBubbleComponent(),
  });
  g.configureClientUiConversationRoot(React, primitives);
  g.configureClientUiConversationSession(React);
  g.configureClientUiConversationDetailsPanel(React, primitives, runtime.shallowEqual);
  g.configureClientUiConversationEnterBehavior(React, primitives);
  g.configureClientUiConversationInputBar(React, primitives, attachment);
  g.configureClientUiConversationQueueDock(React, primitives);
  g.configureClientUiConversationStatsLine(React, primitives);
  g.configureClientUiConversationTodoPanel(React, primitives);
  g.configureClientUiConversationApprovalPanel(React, primitives, g.PendingApproval);
  const components = {
    ConversationRoot: g.conversationRootComponent(),
    ConversationSession: g.conversationSessionComponent(),
    ConversationSessionHeader: g.conversationSessionHeaderComponent(),
    InputBar: g.inputBarComponent(), ApprovalPanel: g.approvalPanelComponent(),
    ChatView: g.chatViewComponent(), StatsLine: g.statsLineComponent(),
    DetailsPanel: g.detailsPanelComponent(), EnterBehaviorRow: g.enterBehaviorRowComponent(),
    todoDockEntry: g.todoDockEntry(), queueDockEntry: g.queueDockEntry(),
    UserMessageNodeView: g.userMessageNodeViewComponent(),
    ContextMessageNodeView: g.contextMessageNodeViewComponent(),
    AssistantNodeView: g.assistantNodeViewComponent(),
    CommandNodeView: g.commandNodeViewComponent(),
    ManualCompactionNodeView: g.manualCompactionNodeViewComponent(),
    CompactionNodeView: g.compactionNodeViewComponent(),
    RetryNodeView: g.retryNodeViewComponent(), TurnErrorNodeView: g.turnErrorNodeViewComponent(),
    TurnMaxTokensNodeView: g.turnMaxTokensNodeViewComponent(),
    TurnTailNodeView: g.turnTailNodeViewComponent(), UnknownNodeView: g.unknownNodeViewComponent(),
  };
  g.configureClientUiConversationApply(
    components,
    runtime.defineStore,
    () => globalThis.crypto.randomUUID(),
  );
  return {
    apply: g.applyClientUiConversation,
    inject: ['slots', 'layout', 'sessions', 'workspaces', 'locale', 'connection', 'remote', 'settingsScope', 'conversationEvents', 'conversationViews'],
    ConversationController: g.ConversationController,
  };
}"
        .replace("__GLOBAL__", global)
}

fn ui_tool_module_factory(global: &str) -> String {
    format!(
        "require => {{ {global}.configureClientUiTool(require('react'), require('@seekdeep-ai/seekdeep-client-ui-primitives')); return {{ apply: {global}.applyClientUiTool, inject: ['slots'] }}; }}"
    )
}

fn ui_settings_models_module_factory(global: &str) -> String {
    format!(
        "require => {{ const webReact = require('@seekdeep-ai/seekdeep-client-web-react'); {global}.configureClientUiSettingsModels(require('react'), require('@seekdeep-ai/seekdeep-client-ui-primitives'), require('@seekdeep-ai/seekdeep-client-schema-form'), webReact.bindSnapshotSelector); return {{ apply: {global}.applyClientUiSettingsModels, inject: ['slots', 'locale', 'connection', 'remote'], refreshIfLoaded: {global}.refreshModelsIfLoaded }}; }}"
    )
}

fn ui_settings_plugins_module_factory(global: &str) -> String {
    format!(
        "require => {{ const slots = require('@seekdeep-ai/seekdeep-client-ui-slots'); const clsx = (...values) => {global}.settingsPluginsClassNames(values); {global}.configureClientUiSettingsPlugins(require('react'), clsx, require('@seekdeep-ai/seekdeep-client-ui-primitives'), slots.resolveSlotLabel); return {{ apply: {global}.applyClientUiSettingsPlugins, inject: ['slots', 'locale', 'connection', 'remote', 'settingsScope'] }}; }}"
    )
}

fn ui_agent_preset_module_factory(global: &str) -> String {
    format!(
        "require => {{ {global}.configureClientUiAgentPreset(require('react'), require('@seekdeep-ai/seekdeep-client-ui-primitives')); Object.assign({global}, {{ apply: {global}.applyClientUiAgentPreset, inject: ['slots', 'locale', 'connection', 'remote'], AGENT_PRESET_SETTINGS_NS: 'agent-presets', writeDefaultPreset: {global}.writeAgentPresetDefault, draftBlocker: {global}.agentPresetDraftBlocker }}); return {global}; }}"
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
    if module_id == "@seekdeep-ai/seekdeep-client-ui-agent-preset" {
        return ui_agent_preset_declarations();
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-settings-plugins" {
        return ui_settings_plugins_declarations();
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
    if module_id == "@seekdeep-ai/seekdeep-client-ui-workspace" {
        return ui_workspace_declarations();
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-directory-picker-browse" {
        return ui_directory_picker_browse_declarations();
    }
    if module_id == "@seekdeep-ai/seekdeep-session-log-export" {
        return session_log_export_declarations();
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-conversation" {
        return ui_conversation_declarations();
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-tool" {
        return ui_tool_declarations();
    }
    if module_id == "@seekdeep-ai/seekdeep-client-ui-settings-models" {
        return ui_settings_models_declarations();
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

#[allow(clippy::too_many_lines)] // Public command UI contract is one closed declaration surface.
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

#[allow(clippy::too_many_lines)] // Public Workspace UI contract is one closed declaration surface.
fn ui_workspace_declarations() -> String {
    r"
import type { SessionId, SessionSearchResultItem, WorkspaceId, WorkspaceView } from '@seekdeep-ai/seekdeep-client-runtime/client';
import type { HostObservable, PropsLocale, PropsRenderSlots, PropsRuntime, SnapshotSelectorHook } from '@seekdeep-ai/seekdeep-client-ui-slots';
export const apply: typeof wasm_bindgen.applyClientUiWorkspace;
export const inject: readonly ['slots', 'sessions', 'workspaces', 'locale'];
export const FLAT_SESSION_ORDER_KEY: '__flat_session_order__';
export interface DirectoryFlowOwnerProps {
  open: boolean;
  busy: boolean;
  onPicked(path: string): void;
  onCancel(): void;
  onError(message: string): void;
}
export type DirectoryFlowSlotName = 'conversation.hero.workspace.directoryFlow' | 'sidebar.workspaces.directoryFlow';
export interface DirectoryPickingInjected { hooks: { directoryFlow: HostObservable<boolean> } }
export interface DirectoryPickingHooks { useDirectoryFlow: SnapshotSelectorHook<boolean> }
export interface WorkspaceBrowserInjected extends DirectoryPickingInjected {
  startSession(workspaceId?: WorkspaceId): void;
  open(sessionId: SessionId): void;
  searchSessions(query: string, signal: AbortSignal): Promise<{ items: readonly SessionSearchResultItem[]; hasMore: boolean }>;
  searchResultLimit: number;
  renameSession(sessionId: SessionId, title: string): Promise<void>;
  forkSession(sessionId: SessionId): void;
  renameWorkspace(workspaceId: WorkspaceId, title: string): Promise<void>;
  deleteWorkspace(workspaceId: WorkspaceId): Promise<void>;
  insertWorkspaceBefore(workspaceId: WorkspaceId, beforeWorkspaceId?: WorkspaceId): Promise<void>;
  archiveSession(sessionId: SessionId): Promise<void>;
  insertSessionBefore(workspaceId: WorkspaceId, sessionId: SessionId, beforeSessionId?: SessionId): Promise<void>;
  createWorkspace(input: { path: string }): Promise<WorkspaceView>;
}
export interface WorkspaceViewState {
  groupBy: 'workspace' | 'flat';
  orderBy: 'manual' | 'updated';
  groupExpansion: Readonly<Record<string, boolean>>;
  sessionOrderByAccount: Readonly<Record<string, readonly string[]>>;
  sessionUpdatedAtByAccount: Readonly<Record<string, Readonly<Record<string, number>>>>;
}
export interface WorkspaceViewActions {
  setGroupBy(mode: 'workspace' | 'flat'): void;
  setOrderBy(mode: 'manual' | 'updated'): void;
  setGroupExpanded(key: string, expanded: boolean): void;
  retainAccountKeys(keys: readonly string[]): void;
  syncSessionOrderAccount(key: string, order: string[], updatedAt: Record<string, number>): void;
  setSessionOrder(key: string, order: string[]): void;
}
export type WorkspaceBrowserProps = PropsRuntime<'sidebar.workspaces'>
  & PropsRenderSlots<'sidebar.workspaces.directoryFlow'>
  & PropsLocale<'workspace'>
  & Omit<WorkspaceBrowserInjected, 'hooks'>
  & DirectoryPickingHooks
  & { useStore: SnapshotSelectorHook<WorkspaceViewState>; actions: WorkspaceViewActions };
export interface WorkspacePickerInjected extends DirectoryPickingInjected {
  createWorkspace(input: { path: string }): Promise<WorkspaceView>;
}
export type WorkspacePickerProps = PropsRuntime<'conversation.hero.workspace'>
  & PropsRenderSlots<'conversation.hero.workspace.directoryFlow'>
  & PropsLocale<'workspace'>
  & Omit<WorkspacePickerInjected, 'hooks'>
  & DirectoryPickingHooks;
export type WorkspaceKey =
  | 'group.ungrouped' | 'session.new' | 'section.workspaces' | 'section.sessions'
  | 'viewOptions.label' | 'groupBy.label' | 'groupBy.workspace' | 'groupBy.flat'
  | 'orderBy.label' | 'orderBy.manual' | 'orderBy.updated' | 'sessions.expand'
  | 'sessions.collapse' | 'empty.none' | 'empty.noMatches' | 'workspace.add'
  | 'search.sessions.aria' | 'search.placeholder' | 'search.clear' | 'search.results.aria'
  | 'search.pending' | 'search.unavailable' | 'search.noMatches' | 'search.hasMore'
  | 'menu.addWorkspace' | 'picker.loading' | 'conflict.named' | 'folderError.title'
  | 'folderError.retry' | 'rename' | 'rename.workspace.title' | 'rename.session.title'
  | 'field.workspaceName' | 'field.sessionName' | 'delete.workspace' | 'delete.desc'
  | 'delete.pending' | 'menu.fork' | 'menu.archiveSession' | 'sessions.count.one'
  | 'sessions.count.other' | 'actions.workspace.aria' | 'actions.session.aria'
  | 'actions.newSession.aria' | 'status.running' | 'status.subagentsRunning.one'
  | 'status.subagentsRunning.other' | 'status.idle' | 'status.waitingApproval'
  | 'status.planReview' | 'status.waitingAnswer' | 'status.completed' | 'hover.created'
  | 'hover.copied' | 'date.ymd' | 'time.now' | 'time.minutes' | 'time.hours'
  | 'time.days' | 'time.months' | 'time.years' | 'time.ago';
declare module '@seekdeep-ai/seekdeep-client-ui-slots' {
  interface LocaleNamespaceMap { workspace: WorkspaceKey }
  interface SlotMap {
    'conversation.hero.workspace.directoryFlow': { kind: 'single'; scope: 'root'; owner: DirectoryFlowOwnerProps };
    'sidebar.workspaces.directoryFlow': { kind: 'single'; scope: 'root'; owner: DirectoryFlowOwnerProps };
  }
}
"
    .to_owned()
}

fn ui_directory_picker_browse_declarations() -> String {
    r"
import type { DirectoryEntry, DirectoryListing } from '@seekdeep-ai/seekdeep-client-runtime/client';
import type { DirectoryFlowOwnerProps } from '@seekdeep-ai/seekdeep-client-ui-workspace/client';
export const apply: typeof wasm_bindgen.applyClientUiDirectoryPickerBrowse;
export const inject: readonly ['slots', 'workspaces', 'locale'];
export interface BrowseFlowInjected {
  listDirectory(path?: string, signal?: AbortSignal): Promise<DirectoryListing>;
  createDirectory(path: string, name: string): Promise<string>;
  t(key: DirectoryBrowserKey, values?: Record<string, unknown>): string;
}
export interface DirectoryBrowserProps extends BrowseFlowInjected {
  open: boolean;
  busy: boolean;
  onOpen(path: string): void;
  onClose(): void;
}
export type BrowseDirectoryFlowProps = DirectoryFlowOwnerProps & BrowseFlowInjected;
export type DirectoryBrowserKey =
  | 'browser.title' | 'browser.home' | 'browser.newFolder' | 'browser.folderName'
  | 'browser.createIn' | 'browser.untitledFolder' | 'browser.create' | 'browser.cancel'
  | 'browser.open' | 'browser.editPath' | 'browser.loading' | 'browser.truncated'
  | 'browser.showHidden';
export interface DirectoryBrowserStateController {
  snapshot(): unknown;
  dispatch(action: string, payload?: unknown): unknown;
}
export const createDirectoryBrowserStateController: () => DirectoryBrowserStateController;
declare module '@seekdeep-ai/seekdeep-client-ui-slots' {
  interface LocaleNamespaceMap { 'directory-browser': DirectoryBrowserKey }
}
"
    .to_owned()
}

fn session_log_export_declarations() -> String {
    r"
import type { ObservableSnapshot, SessionId } from '@seekdeep-ai/seekdeep-client-runtime/client';
import type { PropsLocale, PropsRuntime } from '@seekdeep-ai/seekdeep-client-ui-slots';
export const apply: typeof wasm_bindgen.applySessionLogExport;
export const inject: readonly ['slots', 'locale'];
export type SessionLogDownloadStatus = 'downloading' | 'success' | 'error';
export interface SessionLogDownloadEntry { readonly open: boolean; readonly status: SessionLogDownloadStatus; readonly error: string | null }
export interface SessionLogDownloadState { bySession: Record<string, SessionLogDownloadEntry | undefined> }
export interface SessionLogDownloadDialogInjected {
  hooks: { sessionLogDownload: ObservableSnapshot<SessionLogDownloadState> };
  request(sessionId: SessionId): Promise<void>;
  dismiss(sessionId: SessionId): void;
}
export type SessionLogDownloadKey =
  | 'dialog.preparingTitle' | 'dialog.preparingDescription'
  | 'dialog.successTitle' | 'dialog.successDescription'
  | 'dialog.errorTitle' | 'dialog.commandFailed' | 'dialog.close';
export type SessionLogDownloadDialogProps = PropsRuntime<'conversation.session.header.utilities'>
  & PropsLocale<'session-log-download'>
  & SessionLogDownloadDialogInjected;
export interface SnapshotStore<T> { getSnapshot(): T; subscribe(listener: () => void): () => void; set(value: T): void }
export class SessionLogDownloadController {
  constructor(fetcher?: Function, saver?: Function);
  readonly store: SnapshotStore<SessionLogDownloadState>;
  download(sessionId: SessionId): Promise<void>;
  dismiss(sessionId: SessionId): void;
  dispose(): Promise<void>;
}
export const sessionLogZipFilename: (sessionId: SessionId) => string;
export const downloadUrl: (url: string, filename: string) => void;
declare module '@seekdeep-ai/cordis' { interface Context { sessionLogDownload: SessionLogDownloadController } }
declare module '@seekdeep-ai/seekdeep-client-ui-slots' {
  interface LocaleNamespaceMap { 'session-log-download': SessionLogDownloadKey }
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

fn ui_settings_plugins_declarations() -> String {
    r"
import type { HostObservable, InjectFace, PropsLocale, PropsRenderSlots, PropsRuntime } from '@seekdeep-ai/seekdeep-client-ui-slots';
import type { SnapshotStore } from '@seekdeep-ai/seekdeep-client-runtime/client';
export const apply: typeof wasm_bindgen.applyClientUiSettingsPlugins;
export const inject: readonly ['slots', 'locale', 'connection', 'remote', 'settingsScope'];
export type FieldWrite = { kind: 'set'; value: unknown } | { kind: 'clear' };
export interface CardFieldSpec { field: string; format(value: unknown): string; parse(text: string): FieldWrite | undefined }
export interface CardSecretSpec { field: string; write(text: string): Promise<boolean> }
export interface CardFieldState { text: string; overridden: boolean; invalid: boolean }
export interface CardShell { available: boolean; writable: boolean; dirty: boolean; invalid: boolean; saving: boolean; failed: boolean }
export interface CardActions { edit(field: string, text: string): void; resetField(field: string): void; save(): void; discard(): void }
export interface PluginsSettingsTabEntry { id: string; order: number; label: string }
export interface PluginsSettingsSectionInjected { hooks: { tabs: HostObservable<readonly PluginsSettingsTabEntry[]> } }
export type PluginsSettingsSectionProps = PropsRuntime<'settings.section'> & PropsLocale<'settings.plugins'> & PropsRenderSlots<'settings.plugins.tab'> & InjectFace<PluginsSettingsSectionInjected>;
export interface ConfigurablePluginsTabInjected { cardCount: number }
export type ConfigurablePluginsTabProps = PropsRuntime<'settings.plugins.tab'> & PropsLocale<'settings.plugins'> & PropsRenderSlots<'settings.plugin.item'> & InjectFace<ConfigurablePluginsTabInjected>;
export interface PluginCardProps { t(key: PluginsSettingsLocaleKey): string; titleKey: PluginsSettingsLocaleKey; descriptionKey: PluginsSettingsLocaleKey; state: CardShell; onSave(): void; onDiscard(): void; children: unknown }
export interface FieldProps { id: string; label: string; hint: string; text: string; overridden: boolean; invalid: boolean; overriddenLabel: string; resetLabel: string; invalidLabel: string; disabled: boolean; onEdit(text: string): void; onReset(): void }
export interface AgentLoopCardState extends CardShell { maxParallelToolCalls: CardFieldState }
export interface BashCardState extends CardShell { timeoutMs: CardFieldState; maxOutputBytes: CardFieldState }
export interface WebSearchCardState extends CardShell { baseURL: CardFieldState; maxUses: CardFieldState; apiKey: CardFieldState; apiKeyConfigured: boolean; apiKeyWritable: boolean }
export interface AgentLoopCardFace extends CardActions { hooks: { agentLoopCard: SnapshotStore<AgentLoopCardState> } }
export interface BashCardFace extends CardActions { hooks: { bashCard: SnapshotStore<BashCardState> } }
export interface WebSearchCardFace extends CardActions { hooks: { webSearchCard: SnapshotStore<WebSearchCardState> } }
export interface SettingsPluginItemOwnerProps { children?: never }
export type PluginsSettingsLocaleKey =
  | 'nav' | 'title' | 'intro' | 'tabs' | 'configurableTab' | 'empty'
  | 'overridden' | 'reset' | 'readOnly' | 'expand' | 'collapse'
  | 'save' | 'saving' | 'discard' | 'unsaved' | 'saveFailed' | 'invalidNumber'
  | 'bashTitle' | 'bashDescription' | 'bashTimeoutMs' | 'bashTimeoutMsHint'
  | 'bashMaxOutputBytes' | 'bashMaxOutputBytesHint'
  | 'agentLoopTitle' | 'agentLoopDescription' | 'agentLoopMaxParallel' | 'agentLoopMaxParallelHint'
  | 'webSearchTitle' | 'webSearchDescription' | 'webSearchApiKey' | 'webSearchApiKeyHint'
  | 'webSearchApiKeySet' | 'webSearchApiKeyUnset' | 'webSearchBaseUrl'
  | 'webSearchBaseUrlHint' | 'webSearchMaxUses' | 'webSearchMaxUsesHint';
declare module '@seekdeep-ai/seekdeep-client-ui-slots' {
  interface LocaleNamespaceMap { 'settings.plugins': PluginsSettingsLocaleKey }
  interface SlotMap { 'settings.plugin.item': { kind: 'list'; scope: 'root'; owner: SettingsPluginItemOwnerProps } }
}
"
    .to_owned()
}

fn ui_agent_preset_declarations() -> String {
    r"
import type { SnapshotStore } from '@seekdeep-ai/seekdeep-client-runtime/client';
export const apply: typeof wasm_bindgen.applyClientUiAgentPreset;
export const inject: readonly ['slots', 'locale', 'connection', 'remote'];
export const AGENT_PRESET_SETTINGS_NS: 'agent-presets';
export function writeDefaultPreset(api: unknown, id: string): Promise<string | undefined>;
export function draftBlocker(draft: CopyDraft, rows: readonly PresetRow[]): 'idRequired' | 'idInvalid' | 'idTaken' | undefined;
export type PresetTrust = 'system' | 'user';
export interface AgentPresetOption { id: string; trust: PresetTrust; name?: string; description?: string }
export interface PresetRow extends AgentPresetOption { isDefault: boolean; broken?: string }
export interface CopyDraft { from: string; fromTitle: string; id: string; name: string; saving: boolean; error: string | null }
export interface PresetView { id: string; title: string; content: string }
export interface AgentPresetSettingsState { status: 'idle' | 'loading' | 'ready' | 'saving' | 'unavailable' | 'error'; error: string | null; writable: boolean; currentValue: string; options: readonly AgentPresetOption[] }
export interface AgentPresetSeatState { options: readonly AgentPresetOption[]; current: string; error: string | null; busy: boolean; introduce: boolean }
export interface AgentPresetSectionState { status: 'idle' | 'loading' | 'ready' | 'unavailable' | 'error'; error: string | null; authorable: boolean; hasDocument: boolean; rows: readonly PresetRow[]; copy: CopyDraft | null; view: PresetView | null; pendingDelete: string | null; deleting: boolean; revealedPaths: Readonly<Record<string, string>> }
export interface AgentPresetRowInjected { hooks: { agentPreset: SnapshotStore<AgentPresetSettingsState> }; load(): Promise<void>; select(id: string): Promise<void> }
export interface AgentPresetSeatInjected { hooks: { agentPresetSeat: SnapshotStore<AgentPresetSeatState> }; load(): Promise<void>; select(id: string): Promise<void>; introduced(): void }
export interface AgentPresetLabelInjected { hooks: { agentPresets: SnapshotStore<AgentPresetSettingsState> }; load(): Promise<void> }
export interface AgentPresetSectionInjected { hooks: { agentPresetSection: SnapshotStore<AgentPresetSectionState> }; load(): Promise<void>; view(id: string): Promise<void>; closeView(): void; beginCopy(id: string): void; cancelCopy(): void; setCopyId(id: string): void; setCopyName(name: string): void; confirmCopy(): Promise<void>; openLocation(id: string): Promise<void>; startCreatorDraft?(): void; confirmDelete(id: string | null): void; remove(): Promise<void>; makeDefault(id: string): Promise<void> }
export type AgentPresetSettingsKey = string;
declare module '@seekdeep-ai/seekdeep-client-ui-slots' { interface LocaleNamespaceMap { 'settings.agentPreset': AgentPresetSettingsKey } }
"
    .to_owned()
}

fn ui_conversation_declarations() -> String {
    r"
export const apply: typeof wasm_bindgen.applyClientUiConversation;
export const inject: readonly ['slots', 'layout', 'sessions', 'workspaces', 'locale', 'connection', 'remote', 'settingsScope', 'conversationEvents', 'conversationViews'];
export const ConversationController: typeof wasm_bindgen.ConversationController;
export type DraftAttachmentId = string & { readonly __draftAttachmentId: unique symbol };
export type CallId = string & { readonly __callId: unique symbol };
export interface SelectionTarget { turnSeq: number; stepSeq?: number; callId?: CallId; toolName?: string }
export interface ChatStoreState { selection: SelectionTarget | null; draft: string; view: string | null; inspect: CallId | null }
export interface ViewTab { id: string; label: string }
export type ConversationKey = string;
export type ChatNodeKind = string;
export interface ChatNodeDataMap {
  user: unknown; steering: unknown; context: unknown; 'assistant-step': AssistantChatData;
  command: unknown; 'manual-compaction': ManualCompactionChatData; compaction: unknown;
  'model-retry': RetryChatData; 'turn-error': unknown; 'turn-max-tokens': unknown;
  'turn-tail': TurnTailChatData; unknown: unknown;
}
export interface ChatNode<Kind extends keyof ChatNodeDataMap = keyof ChatNodeDataMap> {
  key: string; kind: Kind; id: string; target: 'chat'; anchorSeq: number;
  location: unknown; visibility: 'visible' | 'hidden'; data: ChatNodeDataMap[Kind];
}
export interface AssistantChatData {
  status: 'running' | 'settled' | 'interrupted'; turn: number; step: number;
  blocks: readonly unknown[]; time: number; usage?: unknown; finalNode?: unknown;
}
export interface ToolChatData { root: unknown }
export interface ManualCompactionChatData { command: unknown; compaction: unknown | null }
export interface RetryChatData { attempts: readonly unknown[]; current: unknown }
export interface TurnTailChatData {
  turn: number; seq: number; time: number; closing: AssistantChatData | null;
  branchUnavailable: boolean; ttftMs?: number; tokensPerSecond?: number;
}
export interface IConversation {
  readonly input: unknown; readonly blocks: unknown;
  send(text: string): Promise<void>; cancel(): Promise<void>; loadOlder(): Promise<void>;
  updateQueue(itemId: string, action: unknown): Promise<void>;
}
export type ChatFileMentions = Record<string, unknown>;
export type ChatNodeOwnerProps = Record<string, unknown>;
export type ChatNodeViewProps = Record<string, unknown>;
export type ChatStore = Record<string, unknown>;
export type ChatViewInjected = Record<string, unknown>;
export type ChatViewSlotProps = Record<string, unknown>;
export type CommandRowOwnerProps = Record<string, unknown>;
export type CommandRowProps = Record<string, unknown>;
export type ComposerBarInjected = Record<string, unknown>;
export type ComposerAttachment = Record<string, unknown>;
export type ComposerChainProps = Record<string, unknown>;
export type ConversationInjected = Record<string, unknown>;
export type ConversationSessionHeaderInjected = Record<string, unknown>;
export type ConversationSessionInjected = Record<string, unknown>;
export type ConversationSlotProps = Record<string, unknown>;
export type ConvViewOwnerProps = Record<string, unknown>;
export type ConvViewProps = Record<string, unknown>;
export type DetailsInjected = Record<string, unknown>;
export type DetailsSlotProps = Record<string, unknown>;
export type DetailsToolOwnerProps = Record<string, unknown>;
export type EmptyWorkspaceOwnerProps = Record<string, unknown>;
export type TurnTailOwnerProps = Record<string, unknown>;
export type UseChatNodeTurnData = (key: string) => unknown;
declare module '@seekdeep-ai/cordis' { interface Context { conversation: IConversation } }
declare module '@seekdeep-ai/seekdeep-client-ui-slots' {
  interface LocaleNamespaceMap { conversation: ConversationKey }
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

fn ui_tool_declarations() -> String {
    r"
import type { ToolCallBlock } from '@seekdeep-ai/seekdeep-client-runtime/client';
import type { PropsLocale, PropsRenderSlots, PropsRuntime } from '@seekdeep-ai/seekdeep-client-ui-slots';
import type {} from '@seekdeep-ai/seekdeep-client-ui-conversation/client';
import type {} from '@seekdeep-ai/seekdeep-client-locale/client';
export const apply: typeof wasm_bindgen.applyClientUiTool;
export const inject: readonly ['slots'];
export interface ToolCallOwnerProps {
  callId: string;
  toolName: string;
  block: ToolCallBlock;
  cwd?: string | undefined;
  openFile(path: string): void;
  inspect?: (() => void) | undefined;
}
export type ToolCallViewProps = PropsRuntime<'tool.call.toolview'>;
export type ToolTreeProps = PropsRuntime<'conversation.chat.node', 'tool-call'>
  & PropsRenderSlots<'tool.call.toolview'>
  & PropsLocale<'conversation'>;
export type ToolDetailsProps = PropsRuntime<'conversation.details.tool'> & PropsLocale<'conversation'>;
declare module '@seekdeep-ai/seekdeep-client-ui-slots' {
  interface SlotMap {
    'tool.call.toolview': { kind: 'keyed'; scope: 'session'; owner: ToolCallOwnerProps };
  }
}
"
    .to_owned()
}

fn ui_settings_models_declarations() -> String {
    r"
import type { ConfigurableProviderView, CredentialView, IApiClient, SettingsNamespaceView } from '@seekdeep-ai/seekdeep-api-remotes/client';
import type { SnapshotSelectorHook } from '@seekdeep-ai/seekdeep-client-web-react';
import type { SnapshotStore } from '@seekdeep-ai/seekdeep-client-runtime/client';
export const apply: typeof wasm_bindgen.applyClientUiSettingsModels;
export const inject: readonly ['slots', 'locale', 'connection', 'remote'];
export const refreshIfLoaded: typeof wasm_bindgen.refreshModelsIfLoaded;
export interface ProviderRow {
  entry: ConfigurableProviderView;
  configured: boolean;
  removable: boolean;
  apiKeyEnv: string | undefined;
  credential: CredentialView | undefined;
}
export interface ModelsSettingsState {
  status: 'idle' | 'loading' | 'ready' | 'error';
  error: string | null;
  credentialError: string | null;
  writable: boolean;
  rows: readonly ProviderRow[];
  namespaces: ReadonlyMap<string, SettingsNamespaceView>;
}
interface ModelsSettingsStore {
  readonly store: SnapshotStore<ModelsSettingsState>;
  load(): Promise<void>;
}
export interface ModelsSectionInjected {
  controller: ModelsSettingsStore;
  useSnapshot: SnapshotSelectorHook<ModelsSettingsState>;
  api: Pick<IApiClient, 'settings' | 'credentials' | 'llm'>;
  t(key: ModelsKey): string;
}
export type ModelsSectionProps = Partial<ModelsSectionInjected>;
export type ModelsKey =
  | 'nav' | 'title' | 'intro' | 'edit' | 'editProvider' | 'remove' | 'removeProvider'
  | 'deleteTitle' | 'deleteDescription' | 'deleteDescriptionWithCredential' | 'deleteConfirm'
  | 'deleting' | 'add' | 'provider' | 'close' | 'cancel' | 'apply' | 'applying'
  | 'savedProvider' | 'credentialConfigured' | 'credentialMissing' | 'readOnly' | 'loadFailed'
  | 'conflict' | 'retry' | 'keyInput' | 'keyPlaceholder' | 'keyPlaceholderNative'
  | 'codexOAuth' | 'keyStored' | 'keyEnvLocked' | 'customized' | 'baseUrl' | 'baseUrlDefault'
  | 'models' | 'modelsInherited' | 'modelsCustomized' | 'resetModels' | 'model' | 'modelId'
  | 'modelName' | 'modelNamePlaceholder' | 'contextWindow' | 'contextWindowPlaceholder'
  | 'maxTokens' | 'maxTokensPlaceholder' | 'modelAdvanced' | 'addModel' | 'removeModel'
  | 'modelsEmpty' | 'keyBlank' | 'keyBlankNew' | 'keyIllegalCharacters' | 'modelIdRequired'
  | 'modelIdDuplicate' | 'modelNameInvalid' | 'modelContextInvalid' | 'modelMaxTokensInvalid'
  | 'advancedHint' | 'modelCapacityInvalid' | 'modelDuplicate' | 'modelContextWindow'
  | 'modelMaxTokens' | 'fetchModels' | 'fetching' | 'fetchNeedsBaseUrl' | 'fetchEmpty'
  | 'fetchTitle' | 'fetchDescription' | 'fetchAdopt' | 'customAdd' | 'customTitle'
  | 'customTag' | 'customRoute' | 'customRouteHint' | 'customRouteInvalid' | 'customRouteTaken'
  | 'customDisplayName' | 'customApi' | 'customApiUnset' | 'customNeedsBaseUrl'
  | 'customNeedsModels' | 'create' | 'creating' | 'welcomeTitle' | 'welcomeBody'
  | 'welcomeContinue' | 'welcomeError' | 'onboardingTitle' | 'onboardingDescription'
  | 'onboardingLater' | 'onboardingSave' | 'onboardingSaving' | 'keyRequired';
declare module '@seekdeep-ai/seekdeep-client-ui-slots' {
  interface LocaleNamespaceMap { 'settings.models': ModelsKey }
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
    for entry in walkdir::WalkDir::new(".")
        .into_iter()
        .filter_entry(|entry| {
            !entry
                .path()
                .components()
                .any(|part| matches!(part.as_os_str().to_str(), Some(".git" | "target")))
                && !is_generated_output(entry.path())
        })
    {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
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

fn is_generated_output(path: &Path) -> bool {
    let parts = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .filter(|component| *component != ".")
        .collect::<Vec<_>>();
    (parts.len() >= 4 && parts[0] == "packages" && parts[3] == "lib")
        || parts.starts_with(&["support", "browser-dependencies", "node_modules"])
        || parts.starts_with(&["python", "sdk", ".venv"])
        || parts.starts_with(&["python", "sdk-runtime", ".venv"])
        || parts.starts_with(&[".wheel-smoke"])
        || (parts.len() >= 3 && parts[0] == "vendor" && parts[2] == "lib")
        || (parts.len() >= 3
            && parts[0] == "apps"
            && parts[1] == "web"
            && matches!(parts[2], "dist" | "generated"))
        || parts.starts_with(&[
            "python",
            "sdk-runtime",
            "src",
            "deepseek_harness_runtime",
            "runtime",
            "node",
        ])
        || matches!(
            parts.as_slice(),
            ["python", "sdk-runtime", "hatch_build.py"]
                | [
                    "python",
                    "sdk",
                    "src",
                    "deepseek_harness",
                    "__init__.py" | "api.py" | "client.py" | "models.py" | "errors.py"
                ]
                | [
                    "python",
                    "sdk-runtime",
                    "src",
                    "deepseek_harness_runtime",
                    "__init__.py" | "_bridge.py"
                ]
        )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        classic_module_bundle, client_loader_esm_wrapper, client_modules_esm_wrapper,
        client_test_runtime_esm_declarations, client_test_runtime_esm_wrapper,
        client_web_esm_declarations, client_web_esm_wrapper, compatibility_declarations,
        compatibility_prelude, copy_ui_attachment_type_declarations,
        copy_ui_primitives_katex_assets, copy_ui_primitives_type_declarations,
        copy_wasm_package_assets, cordis_esm_wrapper, default_macos_platform_tag,
        is_generated_output, is_localization, module_factory, ui_attachment_esm_wrapper,
        ui_attachment_invariant_wrapper, ui_primitives_esm_wrapper,
        ui_primitives_highlight_backend, ui_primitives_internal_wrapper,
        ui_primitives_invariant_wrapper, ui_primitives_markdown_backend, wasm_package_global,
        watch_snapshot, write_wasm_package_compatibility_entries, write_web_frontend,
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
    fn classic_package_globals_include_module_identity_for_shared_artifacts() {
        let connection = wasm_package_global(
            "seekdeep_client_foundation_wasm",
            "@seekdeep-ai/seekdeep-client-connection",
        );
        let gateway = wasm_package_global(
            "seekdeep_client_foundation_wasm",
            "@seekdeep-ai/seekdeep-api-gateway",
        );
        assert_ne!(connection, gateway);
        assert!(super::is_javascript_identifier(&connection));
        assert!(super::is_javascript_identifier(&gateway));
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
    fn ui_settings_plugins_bundle_configures_cards_and_public_contract() {
        let bundle = classic_module_bundle(
            "let wasm_bindgen = {};",
            &[1],
            "__seekdeep_client_ui_settings_plugins_wasm",
            "@seekdeep-ai/seekdeep-client-ui-settings-plugins",
        )
        .unwrap();
        for expected in [
            "configureClientUiSettingsPlugins(require('react')",
            "const clsx = (...values) => __seekdeep_client_ui_settings_plugins_wasm.settingsPluginsClassNames(values)",
            "require('@seekdeep-ai/seekdeep-client-ui-primitives')",
            "slots.resolveSlotLabel",
            "apply: __seekdeep_client_ui_settings_plugins_wasm.applyClientUiSettingsPlugins",
            "inject: ['slots', 'locale', 'connection', 'remote', 'settingsScope']",
        ] {
            assert!(bundle.contains(expected), "missing {expected:?}");
        }
        let declarations =
            compatibility_declarations("@seekdeep-ai/seekdeep-client-ui-settings-plugins");
        for expected in [
            "interface CardShell",
            "interface CardActions",
            "interface PluginsSettingsSectionInjected",
            "type PluginsSettingsSectionProps",
            "interface BashCardState",
            "interface WebSearchCardState",
            "type PluginsSettingsLocaleKey",
            "'settings.plugin.item'",
        ] {
            assert!(declarations.contains(expected), "missing {expected:?}");
        }
        let output = tempfile::tempdir().unwrap();
        write_wasm_package_compatibility_entries(
            "@seekdeep-ai/seekdeep-client-ui-settings-plugins",
            output.path(),
        )
        .unwrap();
        let invariant = std::fs::read_to_string(output.path().join("invariant.js")).unwrap();
        assert!(invariant.contains("client-ui-settings-plugins-invariant"));
    }

    #[test]
    fn ui_agent_preset_bundle_configures_surfaces_and_public_contract() {
        let bundle = classic_module_bundle(
            "let wasm_bindgen = {};",
            &[1],
            "__seekdeep_client_ui_agent_preset_wasm",
            "@seekdeep-ai/seekdeep-client-ui-agent-preset",
        )
        .unwrap();
        for expected in [
            "configureClientUiAgentPreset(require('react')",
            "require('@seekdeep-ai/seekdeep-client-ui-primitives')",
            "apply: __seekdeep_client_ui_agent_preset_wasm.applyClientUiAgentPreset",
            "inject: ['slots', 'locale', 'connection', 'remote']",
            "AGENT_PRESET_SETTINGS_NS: 'agent-presets'",
            "writeDefaultPreset: __seekdeep_client_ui_agent_preset_wasm.writeAgentPresetDefault",
            "draftBlocker: __seekdeep_client_ui_agent_preset_wasm.agentPresetDraftBlocker",
        ] {
            assert!(bundle.contains(expected), "missing {expected:?}");
        }
        let declarations =
            compatibility_declarations("@seekdeep-ai/seekdeep-client-ui-agent-preset");
        for expected in [
            "interface AgentPresetSettingsState",
            "interface AgentPresetSeatState",
            "interface AgentPresetSectionState",
            "interface AgentPresetRowInjected",
            "function writeDefaultPreset",
            "function draftBlocker",
            "'settings.agentPreset'",
        ] {
            assert!(declarations.contains(expected), "missing {expected:?}");
        }
        let output = tempfile::tempdir().unwrap();
        write_wasm_package_compatibility_entries(
            "@seekdeep-ai/seekdeep-client-ui-agent-preset",
            output.path(),
        )
        .unwrap();
        let invariant = std::fs::read_to_string(output.path().join("invariant.js")).unwrap();
        assert!(invariant.contains("client-ui-agent-preset-invariant"));
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
    fn ui_workspace_bundle_configures_browser_apply_and_public_contract() {
        let bundle = classic_module_bundle(
            "let wasm_bindgen = {};",
            &[1],
            "__seekdeep_client_ui_workspace_wasm",
            "@seekdeep-ai/seekdeep-client-ui-workspace",
        )
        .unwrap();
        for expected in [
            "configureClientUiWorkspace(require('react')",
            "require('@seekdeep-ai/seekdeep-client-ui-primitives')",
            "configureClientUiWorkspaceApply(runtime.defineStore)",
            "apply: __seekdeep_client_ui_workspace_wasm.applyClientUiWorkspace",
            "inject: ['slots', 'sessions', 'workspaces', 'locale']",
            "FLAT_SESSION_ORDER_KEY: __seekdeep_client_ui_workspace_wasm.flatSessionOrderKey()",
        ] {
            assert!(bundle.contains(expected), "missing {expected:?}");
        }
        let declarations = compatibility_declarations("@seekdeep-ai/seekdeep-client-ui-workspace");
        for expected in [
            "interface DirectoryFlowOwnerProps",
            "type DirectoryFlowSlotName",
            "interface WorkspaceBrowserInjected",
            "type WorkspaceBrowserProps",
            "interface WorkspacePickerInjected",
            "type WorkspacePickerProps",
            "type WorkspaceKey",
            "'sidebar.workspaces.directoryFlow'",
            "'conversation.hero.workspace.directoryFlow'",
        ] {
            assert!(declarations.contains(expected), "missing {expected:?}");
        }
        let output = tempfile::tempdir().unwrap();
        write_wasm_package_compatibility_entries(
            "@seekdeep-ai/seekdeep-client-ui-workspace",
            output.path(),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(output.path().join("index.js")).unwrap(),
            "export function apply() {}\n"
        );
        let invariant = std::fs::read_to_string(output.path().join("invariant.js")).unwrap();
        assert!(invariant.contains("client-ui-workspace-invariant"));
        assert!(invariant.contains("@seekdeep-ai/seekdeep-client-ui-workspace"));
    }

    #[test]
    fn ui_directory_picker_browse_bundle_configures_flow_and_public_contract() {
        let bundle = classic_module_bundle(
            "let wasm_bindgen = {};",
            &[1],
            "__seekdeep_client_ui_directory_picker_browse_wasm",
            "@seekdeep-ai/seekdeep-client-ui-directory-picker-browse",
        )
        .unwrap();
        for expected in [
            "configureClientUiDirectoryPickerBrowse(require('react')",
            "require('@seekdeep-ai/seekdeep-client-ui-primitives')",
            "apply: __seekdeep_client_ui_directory_picker_browse_wasm.applyClientUiDirectoryPickerBrowse",
            "inject: ['slots', 'workspaces', 'locale']",
        ] {
            assert!(bundle.contains(expected), "missing {expected:?}");
        }
        let declarations =
            compatibility_declarations("@seekdeep-ai/seekdeep-client-ui-directory-picker-browse");
        for expected in [
            "interface BrowseFlowInjected",
            "interface DirectoryBrowserProps",
            "type BrowseDirectoryFlowProps",
            "type DirectoryBrowserKey",
            "interface DirectoryBrowserStateController",
            "'directory-browser'",
        ] {
            assert!(declarations.contains(expected), "missing {expected:?}");
        }
        let output = tempfile::tempdir().unwrap();
        write_wasm_package_compatibility_entries(
            "@seekdeep-ai/seekdeep-client-ui-directory-picker-browse",
            output.path(),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(output.path().join("index.js")).unwrap(),
            "export function apply() {}\n"
        );
        let invariant = std::fs::read_to_string(output.path().join("invariant.js")).unwrap();
        assert!(invariant.contains("client-ui-directory-picker-browse-invariant"));
        assert!(invariant.contains("@seekdeep-ai/seekdeep-client-ui-directory-picker-browse"));
    }

    #[test]
    fn session_log_export_bundle_configures_controller_apply_and_root_command() {
        let bundle = classic_module_bundle(
            "let wasm_bindgen = {};",
            &[1],
            "__seekdeep_session_log_export_wasm",
            "@seekdeep-ai/seekdeep-session-log-export",
        )
        .unwrap();
        for expected in [
            "configureSessionLogExport(require('react')",
            "require('@seekdeep-ai/seekdeep-client-ui-primitives')",
            "class SessionLogDownloadController",
            "configureSessionLogExportApply(SessionLogDownloadController)",
            "apply: g.applySessionLogExport",
            "inject: ['slots', 'locale']",
            "downloadUrl",
        ] {
            assert!(bundle.contains(expected), "missing {expected:?}");
        }
        let declarations = compatibility_declarations("@seekdeep-ai/seekdeep-session-log-export");
        for expected in [
            "type SessionLogDownloadStatus",
            "interface SessionLogDownloadEntry",
            "interface SessionLogDownloadState",
            "interface SessionLogDownloadDialogInjected",
            "class SessionLogDownloadController",
            "const sessionLogZipFilename",
            "'session-log-download'",
        ] {
            assert!(declarations.contains(expected), "missing {expected:?}");
        }
        let output = tempfile::tempdir().unwrap();
        write_wasm_package_compatibility_entries(
            "@seekdeep-ai/seekdeep-session-log-export",
            output.path(),
        )
        .unwrap();
        let index = std::fs::read_to_string(output.path().join("index.js")).unwrap();
        for expected in [
            "name = 'session-log-download'",
            "inject = ['commands']",
            "Download this Session log as a ZIP archive",
            "Session log download requested.",
            "does not accept a path",
        ] {
            assert!(index.contains(expected), "missing {expected:?}");
        }
        let invariant = std::fs::read_to_string(output.path().join("invariant.js")).unwrap();
        assert!(invariant.contains("session-log-export-invariant"));
        assert!(invariant.contains("@seekdeep-ai/seekdeep-session-log-export"));
    }

    #[test]
    fn ui_conversation_bundle_configures_assembly_and_public_contract() {
        let bundle = classic_module_bundle(
            "let wasm_bindgen = {};",
            &[1],
            "__seekdeep_client_ui_conversation_wasm",
            "@seekdeep-ai/seekdeep-client-ui-conversation",
        )
        .unwrap();
        for expected in [
            "configureClientUiConversationReasoning(React, primitives)",
            "configureClientUiConversationMessageItem(React, primitives, attachment",
            "configureClientUiConversationApply(",
            "apply: g.applyClientUiConversation",
            "ConversationController: g.ConversationController",
            "conversationEvents', 'conversationViews'",
        ] {
            assert!(bundle.contains(expected), "missing {expected:?}");
        }
        let declarations =
            compatibility_declarations("@seekdeep-ai/seekdeep-client-ui-conversation");
        for expected in [
            "interface IConversation",
            "interface ChatNodeDataMap",
            "interface AssistantChatData",
            "interface TurnTailChatData",
            "type DraftAttachmentId",
            "ConversationController",
            "interface LocaleNamespaceMap",
        ] {
            assert!(declarations.contains(expected), "missing {expected:?}");
        }
        let output = tempfile::tempdir().unwrap();
        write_wasm_package_compatibility_entries(
            "@seekdeep-ai/seekdeep-client-ui-conversation",
            output.path(),
        )
        .unwrap();
        let invariant = std::fs::read_to_string(output.path().join("invariant.js")).unwrap();
        assert!(invariant.contains("client-ui-conversation-invariant"));
        assert!(invariant.contains("@seekdeep-ai/seekdeep-client-ui-conversation"));
    }

    #[test]
    fn ui_tool_bundle_configures_renderers_and_exact_public_contract() {
        let bundle = classic_module_bundle(
            "let wasm_bindgen = {};",
            &[1],
            "__seekdeep_client_ui_tool_wasm",
            "@seekdeep-ai/seekdeep-client-ui-tool",
        )
        .unwrap();
        for expected in [
            "configureClientUiTool(require('react')",
            "require('@seekdeep-ai/seekdeep-client-ui-primitives')",
            "apply: __seekdeep_client_ui_tool_wasm.applyClientUiTool",
            "inject: ['slots']",
        ] {
            assert!(bundle.contains(expected), "missing {expected:?}");
        }
        let declarations = compatibility_declarations("@seekdeep-ai/seekdeep-client-ui-tool");
        for expected in [
            "interface ToolCallOwnerProps",
            "type ToolCallViewProps",
            "type ToolTreeProps",
            "type ToolDetailsProps",
            "'tool.call.toolview'",
        ] {
            assert!(declarations.contains(expected), "missing {expected:?}");
        }
        let output = tempfile::tempdir().unwrap();
        write_wasm_package_compatibility_entries(
            "@seekdeep-ai/seekdeep-client-ui-tool",
            output.path(),
        )
        .unwrap();
        let invariant = std::fs::read_to_string(output.path().join("invariant.js")).unwrap();
        assert!(invariant.contains("client-ui-tool-invariant"));
        assert!(invariant.contains("@seekdeep-ai/seekdeep-client-ui-tool"));
    }

    #[test]
    fn ui_settings_models_bundle_configures_forms_and_public_contract() {
        let bundle = classic_module_bundle(
            "let wasm_bindgen = {};",
            &[1],
            "__seekdeep_client_ui_settings_models_wasm",
            "@seekdeep-ai/seekdeep-client-ui-settings-models",
        )
        .unwrap();
        for expected in [
            "configureClientUiSettingsModels(require('react')",
            "require('@seekdeep-ai/seekdeep-client-ui-primitives')",
            "require('@seekdeep-ai/seekdeep-client-schema-form')",
            "webReact.bindSnapshotSelector",
            "apply: __seekdeep_client_ui_settings_models_wasm.applyClientUiSettingsModels",
            "refreshIfLoaded: __seekdeep_client_ui_settings_models_wasm.refreshModelsIfLoaded",
        ] {
            assert!(bundle.contains(expected), "missing {expected:?}");
        }
        let declarations =
            compatibility_declarations("@seekdeep-ai/seekdeep-client-ui-settings-models");
        for expected in [
            "interface ProviderRow",
            "interface ModelsSettingsState",
            "interface ModelsSectionInjected",
            "type ModelsSectionProps",
            "type ModelsKey",
            "'settings.models'",
        ] {
            assert!(declarations.contains(expected), "missing {expected:?}");
        }
        let output = tempfile::tempdir().unwrap();
        write_wasm_package_compatibility_entries(
            "@seekdeep-ai/seekdeep-client-ui-settings-models",
            output.path(),
        )
        .unwrap();
        let invariant = std::fs::read_to_string(output.path().join("invariant.js")).unwrap();
        assert!(invariant.contains("client-ui-settings-models-invariant"));
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
            "await init({ module_or_path: new URL('./client_bg.wasm', import.meta.url) })",
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
    fn browser_foundation_wrappers_keep_rust_owned_boot_contracts() {
        let cordis = cordis_esm_wrapper();
        for expected in [
            "await init({ module_or_path:",
            "wasm.configureContextWrapper(wrapContext)",
            "traceService(context, target.get(key))",
            "if (key === 'get') return name => traceService(context, target.get(name))",
            "constructor() { return wasm.createContext(); }",
            "Object.defineProperty(this, SERVICE_TRACKER",
            "ctx.provide(name, this)",
        ] {
            assert!(cordis.contains(expected), "missing Cordis {expected:?}");
        }
        let loader = client_loader_esm_wrapper();
        for expected in [
            "wasm.clientLoaderPlugin()",
            "export const Loader = wasm.WasmClientLoader",
            "export default plugin",
        ] {
            assert!(loader.contains(expected), "missing Loader {expected:?}");
        }
        let modules = client_modules_esm_wrapper();
        for expected in [
            "new wasm.WasmClientModuleSystem(",
            "options.staticModules",
            "export const parseBootManifest",
        ] {
            assert!(modules.contains(expected), "missing Modules {expected:?}");
        }
        let shell = client_web_esm_wrapper();
        for expected in [
            "import * as Immer from 'immer'",
            "'immer': Immer",
            "export const AppWebEntry = wasm.AppWebEntry",
        ] {
            assert!(shell.contains(expected), "missing shell {expected:?}");
        }
        let runtime = compatibility_prelude(
            "__seekdeep_client_runtime_wasm",
            "@seekdeep-ai/seekdeep-client-runtime",
        );
        assert!(runtime.contains("const apply = ctx =>"));
        assert!(runtime.contains("'remote.commands'"));
        let runtime_factory = module_factory(
            "__seekdeep_client_runtime_wasm",
            "@seekdeep-ai/seekdeep-client-runtime",
        );
        assert!(runtime_factory.contains("installStoreProduce(require('immer').produce)"));
        for (module_id, expected) in [
            (
                "@seekdeep-ai/seekdeep-client-connection",
                "clientConnectionPlugin()",
            ),
            (
                "@seekdeep-ai/seekdeep-typert-registry",
                "clientTypertRegistryPlugin()",
            ),
            (
                "@seekdeep-ai/seekdeep-api-gateway",
                "clientApiGatewayPlugin()",
            ),
            ("@seekdeep-ai/seekdeep-client-hmr", "clientHmrPlugin()"),
            (
                "@seekdeep-ai/seekdeep-cordis-client-runner",
                "cordisClientRunnerPlugin(require('react'))",
            ),
            (
                "@seekdeep-ai/seekdeep-client-ui-cordis",
                "clientUiCordisPlugin(require('react')",
            ),
        ] {
            assert!(
                module_factory("__compiled", module_id).contains(expected),
                "missing {module_id} factory {expected:?}"
            );
        }
        assert!(
            module_factory(
                "__seekdeep_client_ui_settings_plugins_wasm",
                "@seekdeep-ai/seekdeep-client-ui-settings-plugins",
            )
            .contains("settingsPluginsClassNames(values)")
        );
    }

    #[test]
    fn client_test_runtime_esm_wrapper_keeps_assembly_and_public_contract() {
        let wrapper = client_test_runtime_esm_wrapper();
        for expected in [
            "wasm.initSync({ module: Uint8Array.from(binary",
            "wasm.configureContextWrapper(wrapContext)",
            "wasm.configureClientTestRuntime({",
            "createContext: () => wasm.createContext()",
            "invokePlugin",
            "resolveInject",
            "registerSnapshotSerializer: registerDomSnapshotSerializer",
            "export class TestSessions",
            "wasm.configureFixtureSessionPrototype(FixtureSession.prototype)",
            "wasm.createTestSessions(stabilizer, rootCtx, produce)",
            "export class TestWorkspaces",
            "export class SlotTestRuntime extends wasm.SlotTestRuntime",
            "export const stubSettingsScope",
            "export const makeTranslate",
            "new wasm.WasmBrowserLanguagePin",
        ] {
            assert!(wrapper.contains(expected), "missing {expected:?}");
        }
        let declarations = client_test_runtime_esm_declarations();
        for expected in [
            "export type Stabilizer",
            "export declare class FixtureSession",
            "export declare class TestSessions",
            "export declare class TestWorkspaces",
            "export declare class TestRoot",
            "export declare class SlotTestRuntime",
            "static create(): Promise<SlotTestRuntime>",
            "export declare const domSnapshotSerializer",
            "export declare function stubSettingsScope<T>()",
            "export declare function makeTranslate",
            "export declare function usePinnedBrowserLanguages",
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
        for name in ["README.md", "README.zh.md", "README.i18n.yaml"] {
            assert_eq!(
                std::fs::read(projected.join(name)).unwrap(),
                std::fs::read(
                    workspace
                        .join("packages/client/ui-primitives/assets/katex")
                        .join(name)
                )
                .unwrap()
            );
        }
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
    fn ui_attachment_esm_library_exports_components_types_and_invariant() {
        let wrapper = ui_attachment_esm_wrapper();
        for expected in [
            "configureClientUiAttachment(React, ReactDOM, UiPrimitives)",
            "export const AttachmentRail = wasm.attachmentRailComponent()",
            "export const DropOverlay = wasm.dropOverlayComponent()",
            "export const ImageLightbox = wasm.imageLightboxComponent()",
            "export const MessageImage = wasm.messageImageComponent()",
            "export const ImageGallery = wasm.imageGalleryComponent()",
        ] {
            assert!(wrapper.contains(expected), "missing {expected:?}");
        }
        assert_eq!(wrapper.matches("export const ").count(), 5);
        assert!(ui_attachment_invariant_wrapper().contains("client-ui-attachment-invariant"));
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask has a workspace parent");
        let output = tempfile::tempdir().unwrap();
        copy_ui_attachment_type_declarations(workspace, output.path()).unwrap();
        assert_eq!(
            walkdir::WalkDir::new(output.path())
                .into_iter()
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry.file_type().is_file()
                        && entry.path().extension().and_then(std::ffi::OsStr::to_str) == Some("ts")
                })
                .count(),
            6
        );
        let index = std::fs::read_to_string(output.path().join("index.d.ts")).unwrap();
        assert!(index.contains("export { AttachmentRail }"));
        assert!(index.contains("export { ImageGallery, MessageImage }"));
        assert!(!index.contains("@deepseek-ai/dsh-"));
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
            "configureApiRemotes(__seekdeep_api_remotes_client_wasm.generatedApiRemotes())",
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
    fn rust_only_gate_skips_only_named_derivative_roots() {
        for environment in [
            "python/sdk/.venv/lib/python3.10/site-packages/typing_extensions.py",
            "python/sdk-runtime/.venv/lib/python3.14/site-packages/hatchling/build.py",
            ".wheel-smoke/lib/python3.10/site-packages/deepseek_harness/__init__.py",
        ] {
            assert!(is_generated_output(Path::new(environment)), "{environment}");
        }
        assert!(is_generated_output(Path::new(
            "packages/client/runtime/lib/client.js"
        )));
        assert!(is_generated_output(Path::new(
            "vendor/cordis/lib/client.js"
        )));
        assert!(is_generated_output(Path::new(
            "apps/web/dist/assets/index.js"
        )));
        assert!(is_generated_output(Path::new(
            "apps/web/generated/vite.config.mjs"
        )));
        assert!(is_generated_output(Path::new(
            "support/browser-dependencies/node_modules/.pnpm/zod@4.4.3/node_modules/zod/index.js"
        )));
        assert!(is_generated_output(Path::new(
            "python/sdk-runtime/src/deepseek_harness_runtime/runtime/node/node_modules/@seekdeep-ai/seekdeep-sdk-jsonrpc-demo/lib/packaged-bin.js"
        )));
        for binding in [
            "python/sdk-runtime/hatch_build.py",
            "python/sdk-runtime/src/deepseek_harness_runtime/__init__.py",
            "python/sdk-runtime/src/deepseek_harness_runtime/_bridge.py",
            "python/sdk/src/deepseek_harness/__init__.py",
            "python/sdk/src/deepseek_harness/api.py",
            "python/sdk/src/deepseek_harness/client.py",
            "python/sdk/src/deepseek_harness/models.py",
            "python/sdk/src/deepseek_harness/errors.py",
        ] {
            assert!(is_generated_output(Path::new(binding)), "{binding}");
        }
        for source in [
            "support/browser-dependencies/src/index.js",
            "support/browser-dependencies/node_modules-source/index.js",
            "support/other/node_modules/index.js",
            "python/sdk/.venv-source/implementation.py",
            "crates/unported/.venv/implementation.py",
            "python/sdk-runtime/src/deepseek_harness_runtime/implementation.py",
            "python/sdk-runtime/src/deepseek_harness_runtime/runtime/node-source/entry.js",
            "python/sdk/src/deepseek_harness/runtime/node/entry.js",
        ] {
            assert!(!is_generated_output(Path::new(source)), "{source}");
        }
        assert!(!is_generated_output(Path::new(
            "packages/client/runtime/src/client.js"
        )));
        assert!(!is_generated_output(Path::new(
            "packages/client/lib/client.js"
        )));
        assert!(!is_generated_output(Path::new("apps/web/src/main.js")));
    }

    #[test]
    fn web_frontend_generation_keeps_the_mount_and_build_policy_rust_owned() {
        let root = tempfile::tempdir().unwrap();
        write_web_frontend(root.path()).unwrap();
        let generated = root.path().join("apps/web/generated");
        let entry = std::fs::read_to_string(generated.join("main.js")).unwrap();
        assert!(entry.contains("import { AppWebEntry }"));
        assert!(entry.contains("new AppWebEntry(root).run()"));
        let config = std::fs::read_to_string(generated.join("vite.config.mjs")).unwrap();
        for expected in [
            "seekdeep-reject-standalone-web-serve",
            "window.__SEEKDEEP_BOOT__",
            "target: 'esnext'",
            "assets/langs/[name]-[hash].js",
            "assets/fonts/[name]-[hash][extname]",
            "process.env.CORDIS_SHARED",
        ] {
            assert!(config.contains(expected), "missing {expected:?}");
        }
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
