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
    let mut previous = watch_snapshot(&package_root)?;
    println!(
        "watching {} for Rust/WASM bundle changes",
        package_root.display()
    );
    loop {
        std::thread::sleep(Duration::from_millis(250));
        let current = watch_snapshot(&package_root)?;
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
    let mut declarations = std::fs::read_to_string(staging.join("client.d.ts"))?;
    declarations.push_str(&compatibility_declarations(module_id));
    std::fs::write(type_dir.join("index.d.ts"), declarations)?;
    println!(
        "built {module_id} Rust/WASM classic bundle at {}",
        out_dir.join("client.js").display()
    );
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

fn module_factory(global: &str, module_id: &str) -> String {
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
    format!("() => {global}")
}

fn compatibility_declarations(module_id: &str) -> String {
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
    if module_id != "@seekdeep-ai/seekdeep-client-runtime" {
        return String::new();
    }
    "\nexport const apply: typeof wasm_bindgen.applyClientRuntime;\nexport const isAppendSurfaceEvent: typeof wasm_bindgen.isAppendSurfaceEvent;\nexport const isReplacementSurfaceEvent: typeof wasm_bindgen.isReplacementSurfaceEvent;\nexport const SlotRegistry: typeof wasm_bindgen.ClientSlotRegistry;\nexport const ConversationEventRegistry: typeof wasm_bindgen.ConversationEventRegistry;\nexport const ConversationViewRegistry: typeof wasm_bindgen.ConversationViewRegistry;\nexport const ConversationNodeAssembler: typeof wasm_bindgen.ConversationNodeAssembler;\nexport const ConversationLocationIndex: typeof wasm_bindgen.ConversationLocationIndex;\nexport const conversationContextKey: typeof wasm_bindgen.conversationContextKey;\nexport const SessionRuntime: typeof wasm_bindgen.SessionRuntime;\nexport const scopeOf: typeof wasm_bindgen.scopeOf;\nexport const workspaceTitleOf: typeof wasm_bindgen.workspaceTitleOf;\nexport const indexSubagentDescendants: typeof wasm_bindgen.indexSubagentDescendants;\nexport const SessionProvideChannel: typeof wasm_bindgen.SessionProvideChannel;\nexport const createScope: typeof wasm_bindgen.createScope;\nexport const WorkspaceRuntime: typeof wasm_bindgen.WorkspaceRuntime;\nexport const resolveWorkspacePath: typeof wasm_bindgen.resolveWorkspacePath;\nexport const createSnapshotStore: typeof wasm_bindgen.createSnapshotStore;\nexport const defineStore: typeof wasm_bindgen.defineStore;\nexport const shallowEqual: typeof wasm_bindgen.shallowEqual;\nexport const toAssistantBlock: typeof wasm_bindgen.toAssistantBlock;\nexport const toAssistantBlocks: typeof wasm_bindgen.toAssistantBlocks;\nexport const emptyAssistantBlock: typeof wasm_bindgen.emptyAssistantBlock;\nexport const isTokenDelta: typeof wasm_bindgen.isTokenDelta;\nexport const contextForm: typeof wasm_bindgen.contextForm;\nexport const contextProvenance: typeof wasm_bindgen.contextProvenance;\nexport const displayFailureMessage: typeof wasm_bindgen.displayFailureMessage;\nexport const PendingWait: typeof wasm_bindgen.PendingWait;\nexport class SessionCreateError extends Error { constructor(rpcError: any, requestedSessionId: string | undefined); readonly rpcError: any; readonly requestedSessionId: string | undefined; }\nexport class SessionForkError extends Error { constructor(rpcError: any, sourceSessionId: string); readonly rpcError: any; readonly sourceSessionId: string; }\nexport class WorkspaceCreateError extends Error { constructor(rpcError: any); readonly rpcError: any; }\nexport class DirectoryBrowseError extends Error { constructor(rpcError: any); readonly rpcError: any; }\nexport const EMPTY_CHAT_SNAPSHOT: ReturnType<typeof wasm_bindgen.emptyChatSnapshot>;\nexport const EMPTY_CONVERSATION_VIEWS: ReturnType<typeof wasm_bindgen.emptyConversationViews>;\n".to_owned()
        + runtime_settings_contract_declarations()
}

fn runtime_settings_contract_declarations() -> &'static str {
    r"export interface SettingsScopeSnapshot<T> {
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
        classic_module_bundle, compatibility_declarations, is_generated_package_output,
        is_localization, watch_snapshot,
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
    fn wasm_watch_snapshot_changes_with_package_input_bytes() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        let source = root.path().join("src/lib.rs");
        std::fs::write(&source, "one\n").unwrap();
        let before = watch_snapshot(root.path()).unwrap();
        std::fs::write(&source, "one two\n").unwrap();
        let after = watch_snapshot(root.path()).unwrap();
        assert_ne!(before, after);
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
        ] {
            assert!(declarations.contains(expected));
        }
        assert!(compatibility_declarations("other").is_empty());
    }
}
