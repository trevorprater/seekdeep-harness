//! Assembled-jsdom snapshot lane: the pinned source suites (`apps/web/tests/*.snapshot.ts`)
//! run unchanged under the source's vitest + jsdom, booting the port's built Rust/WASM
//! plugin bundles through the port's `AppWebEntry` against the in-page Rust fixture
//! transport (`?fixture`). The source's `assembled-boot.ts` scaffolding is test support;
//! the port supplies the same exports over its own bundle map and boot globals.

use std::{path::Path, process::Command};

/// Snapshot suites the source assembled-jsdom lane owns.
const SUITES: &[&str] = &[
    "built-boot",
    "image-display",
    "max-tokens-notice",
    "search-card",
    "todo-row",
];

/// Boot entries of the source's minimal assembled graph with the port's package identities.
const PLUGINS: &[(&str, &str, &str, &[&str], bool)] = &[
    (
        "@seekdeep-ai/seekdeep-typert-registry",
        "packages/typert/registry/lib/client.js",
        "/plugins/typert-registry.js",
        &[],
        true,
    ),
    (
        "@seekdeep-ai/seekdeep-client-connection",
        "packages/client/connection/lib/client.js",
        "/plugins/connection.js",
        &[],
        true,
    ),
    (
        "@seekdeep-ai/seekdeep-api-gateway",
        "packages/api/gateway/lib/client.js",
        "/plugins/api-gateway.js",
        &[
            "@seekdeep-ai/seekdeep-typert-registry",
            "@seekdeep-ai/seekdeep-client-connection",
        ],
        true,
    ),
    (
        "@seekdeep-ai/seekdeep-api-remotes",
        "packages/api/remotes/lib/client.js",
        "/plugins/api-remotes.js",
        &["@seekdeep-ai/seekdeep-api-gateway"],
        true,
    ),
    (
        "@seekdeep-ai/seekdeep-client-ui-settings",
        "packages/client/ui-settings/lib/client.js",
        "/plugins/ui-settings.js",
        &[
            "@seekdeep-ai/seekdeep-client-connection",
            "@seekdeep-ai/seekdeep-client-runtime",
            "@seekdeep-ai/seekdeep-api-remotes",
        ],
        false,
    ),
    (
        "@seekdeep-ai/seekdeep-client-runtime",
        "packages/client/runtime/lib/client.js",
        "/plugins/runtime.js",
        &[
            "@seekdeep-ai/seekdeep-client-connection",
            "@seekdeep-ai/seekdeep-typert-registry",
            "@seekdeep-ai/seekdeep-api-gateway",
        ],
        true,
    ),
    (
        "@seekdeep-ai/seekdeep-client-ui-theme",
        "packages/client/ui-theme/lib/client.js",
        "/plugins/ui-theme.js",
        &[
            "@seekdeep-ai/seekdeep-client-connection",
            "@seekdeep-ai/seekdeep-client-runtime",
            "@seekdeep-ai/seekdeep-client-locale",
            "@seekdeep-ai/seekdeep-client-ui-settings",
        ],
        false,
    ),
    (
        "@seekdeep-ai/seekdeep-client-locale",
        "packages/client/locale/lib/client.js",
        "/plugins/locale.js",
        &[
            "@seekdeep-ai/seekdeep-client-connection",
            "@seekdeep-ai/seekdeep-client-runtime",
            "@seekdeep-ai/seekdeep-client-ui-settings",
            "@seekdeep-ai/seekdeep-api-remotes",
        ],
        false,
    ),
    (
        "@seekdeep-ai/seekdeep-client-ui-layout",
        "packages/client/ui-layout/lib/client.js",
        "/plugins/ui-layout.js",
        &["@seekdeep-ai/seekdeep-client-runtime"],
        false,
    ),
    (
        "@seekdeep-ai/seekdeep-client-ui-sidebar",
        "packages/client/ui-sidebar/lib/client.js",
        "/plugins/ui-sidebar.js",
        &["@seekdeep-ai/seekdeep-client-ui-layout"],
        false,
    ),
    (
        "@seekdeep-ai/seekdeep-client-ui-conversation",
        "packages/client/ui-conversation/lib/client.js",
        "/plugins/ui-conversation.js",
        &["@seekdeep-ai/seekdeep-client-ui-layout"],
        false,
    ),
    (
        "@seekdeep-ai/seekdeep-client-ui-tool",
        "packages/client/ui-tool/lib/client.js",
        "/plugins/ui-tool.js",
        &[
            "@seekdeep-ai/seekdeep-client-runtime",
            "@seekdeep-ai/seekdeep-client-locale",
            "@seekdeep-ai/seekdeep-client-ui-conversation",
        ],
        false,
    ),
    (
        "@seekdeep-ai/seekdeep-client-ui-workflow-run",
        "packages/client/ui-workflow-run/lib/client.js",
        "/plugins/ui-workflow-run.js",
        &[
            "@seekdeep-ai/seekdeep-client-locale",
            "@seekdeep-ai/seekdeep-client-runtime",
            "@seekdeep-ai/seekdeep-client-ui-conversation",
            "@seekdeep-ai/seekdeep-client-ui-tool",
        ],
        false,
    ),
    (
        "@seekdeep-ai/seekdeep-client-ui-workspace",
        "packages/client/ui-workspace/lib/client.js",
        "/plugins/ui-workspace.js",
        &[
            "@seekdeep-ai/seekdeep-client-runtime",
            "@seekdeep-ai/seekdeep-client-ui-conversation",
            "@seekdeep-ai/seekdeep-client-ui-sidebar",
        ],
        false,
    ),
    (
        "@seekdeep-ai/seekdeep-session-log-export",
        "packages/session-query/session-log-export/lib/client.js",
        "/plugins/session-log-download.js",
        &[
            "@seekdeep-ai/seekdeep-client-ui-commands",
            "@seekdeep-ai/seekdeep-client-ui-conversation",
        ],
        false,
    ),
    (
        "@seekdeep-ai/seekdeep-client-ui-trajectory",
        "packages/client/ui-trajectory/lib/client.js",
        "/plugins/ui-trajectory.js",
        &["@seekdeep-ai/seekdeep-client-ui-conversation"],
        false,
    ),
];

/// Bare specifiers the port's foundation ESM wrappers import, resolved to the port's package
/// libraries or the source checkout's pinned dependency store.
fn aliases(root: &Path, source: &Path) -> anyhow::Result<String> {
    let port = |relative: &str| serde_json::to_string(&root.join(relative).to_string_lossy());
    let pnpm = |name: &str| -> anyhow::Result<String> {
        let store = source.join("node_modules/.pnpm");
        let prefix = format!("{name}@");
        let entry = std::fs::read_dir(&store)?
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            // `react-dom@18.3.1_react@18.3.1`: a peer suffix follows the version; the shortest
            // matching entry is the one without extra peers.
            .filter(|entry| entry.starts_with(&prefix))
            .min_by_key(|entry| (entry.len(), entry.clone()))
            .ok_or_else(|| {
                anyhow::anyhow!("pinned dependency {name} missing from the source store")
            })?;
        Ok(serde_json::to_string(
            &store
                .join(entry)
                .join("node_modules")
                .join(name)
                .to_string_lossy(),
        )?)
    };
    let react = pnpm("react")?;
    let react_dom = pnpm("react-dom")?;
    let immer = pnpm("immer")?;
    Ok(format!(
        r"[
  {{ find: /^\.\/assembled-boot\.ts$/, replacement: HARNESS }},
  {{ find: /^@testing-library\/react$/, replacement: {testing_library} }},
  {{ find: /^vitest$/, replacement: {vitest} }},
  {{ find: /^react$/, replacement: {react} }},
  {{ find: /^react\/(.*)$/, replacement: {react} + '/$1' }},
  {{ find: /^react-dom$/, replacement: {react_dom} }},
  {{ find: /^react-dom\/(.*)$/, replacement: {react_dom} + '/$1' }},
  {{ find: /^immer$/, replacement: {immer} }},
  {{ find: /^@seekdeep-ai\/cordis$/, replacement: {cordis} }},
  {{ find: /^@seekdeep-ai\/cordis-plugin-loader$/, replacement: {loader} }},
  {{ find: /^@seekdeep-ai\/seekdeep-client-modules\/client$/, replacement: {modules_client} }},
  {{ find: /^@seekdeep-ai\/seekdeep-client-web$/, replacement: {web} }},
  {{ find: /^@seekdeep-ai\/seekdeep-client-web-react$/, replacement: {web_react} }},
  {{ find: /^@seekdeep-ai\/seekdeep-client-ui-slots$/, replacement: {ui_slots} }},
  {{ find: /^@seekdeep-ai\/seekdeep-client-ui-primitives$/, replacement: {ui_primitives} }},
  {{ find: /^@seekdeep-ai\/seekdeep-client-ui-attachment$/, replacement: {ui_attachment} }},
  {{ find: /^@seekdeep-ai\/seekdeep-client-schema-form$/, replacement: {schema_form} }},
]",
        testing_library = serde_json::to_string(
            &source
                .join("node_modules/@testing-library/react")
                .to_string_lossy(),
        )?,
        vitest = serde_json::to_string(
            &source
                .join("node_modules/vitest/dist/index.js")
                .to_string_lossy(),
        )?,
        cordis = port("vendor/cordis/lib/index.js")?,
        loader = port("vendor/loader/lib/index.js")?,
        modules_client = port("packages/client/modules/lib/client.js")?,
        web = port("packages/client/web/lib/index.js")?,
        web_react = port("packages/client/web-react/lib/index.js")?,
        ui_slots = port("packages/client/ui-slots/lib/index.js")?,
        ui_primitives = port("packages/client/ui-primitives/lib/index.js")?,
        ui_attachment = port("packages/client/ui-attachment/lib/index.js")?,
        schema_form = port("packages/client/schema-form/lib/index.js")?,
    ))
}

fn harness(root: &Path) -> anyhow::Result<String> {
    let entries = PLUGINS
        .iter()
        .map(|(id, bundle, url, inject, immediately)| {
            let inject = inject
                .iter()
                .map(|name| format!("'{name}'"))
                .collect::<Vec<_>>()
                .join(", ");
            let immediately = if *immediately { ", immediately: true" } else { "" };
            format!(
                "  {{ id: '{id}', bundlePath: '{bundle}', url: '{url}', rev: 'fx', inject: [{inject}]{immediately} }},"
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(format!(
        r"// Port of the source `assembled-boot.ts` scaffolding: the same jsdom setup, teardown,
// and mount call over the port's built Rust/WASM bundles and boot globals.
import {{ readFileSync }} from 'node:fs'
import {{ join }} from 'node:path'
import {{ act, cleanup }} from '@testing-library/react'
import {{ afterEach, beforeEach, vi }} from 'vitest'
import {{ AppWebEntry }} from '@seekdeep-ai/seekdeep-client-web'

const PORT_ROOT = {port_root}
const PLUGINS = [
{entries}
]
const bundles = new Map(PLUGINS.map(plugin => [plugin.url, readFileSync(join(PORT_ROOT, plugin.bundlePath), 'utf8')]))

class ResizeObserverStub {{
  observe() {{}}
  disconnect() {{}}
  unobserve() {{}}
}}

const win = window
let unmount

export function installAssembledBootEnv() {{
  beforeEach(() => {{
    localStorage.clear()
    Object.defineProperty(navigator, 'languages', {{ value: ['en-US'], configurable: true }})
    Object.defineProperty(navigator, 'language', {{ value: 'en-US', configurable: true }})
    document.title = 'SeekDeep Harness'
    vi.stubGlobal('ResizeObserver', ResizeObserverStub)
    vi.stubGlobal('requestAnimationFrame', callback => setTimeout(() => {{ callback(0) }}, 0))
    vi.stubGlobal('cancelAnimationFrame', id => {{ clearTimeout(id) }})
  }})

  afterEach(() => {{
    act(() => {{ unmount?.() }})
    unmount = undefined
    cleanup()
    delete win.__SEEKDEEP_BOOT__
    delete win.__ModuleLoader__
    delete win.__fxTiming
    document.body.innerHTML = ''
    document.head.querySelectorAll('style[data-plugin]').forEach(style => {{ style.remove() }})
    document.title = ''
    history.replaceState(null, '', '/')
    const ownNavigator = navigator
    delete ownNavigator.languages
    delete ownNavigator.language
    vi.unstubAllGlobals()
  }})
}}

export function mountAssembledApp() {{
  history.replaceState(null, '', '/?fixture')
  const root = document.createElement('div')
  root.id = 'root'
  document.body.appendChild(root)
  win.__SEEKDEEP_BOOT__ = {{ rev: 'fx', entries: PLUGINS.map(({{ bundlePath: _bundlePath, ...plugin }}) => plugin) }}
  act(() => {{
    const entry = new AppWebEntry(root, {{
      loadBundle: async (url) => {{
        const code = bundles.get(url)
        if (code === undefined) throw new Error(`missing built bundle ${{url}}`)
        ;(0, eval)(code)
      }},
    }})
    void entry.run()
    unmount = () => {{ entry.dispose() }}
  }})
}}

/** Match a module class by its logical name: the port emits `<package>-<component>-<name>`. */
export function hasClass(el, name) {{
  return [...el.classList].some(cls => cls === name || cls.endsWith(`-${{name}}`) || cls.endsWith(`_${{name}}`) || cls.startsWith(`_${{name}}_`) || cls.includes(`_${{name}}_`))
}}

export const REFRESHING_GOLDEN = process.env.SEEKDEEP_SNAPSHOT === 'record' || process.env.SEEKDEEP_SNAPSHOT === 'refresh'
",
        port_root = serde_json::to_string(&root.to_string_lossy())?,
    ))
}

/// Runs the assembled-jsdom snapshot suites (optionally one `suite`) against the port bundles.
///
/// # Errors
///
/// Returns when the source checkout is absent, staging fails, or vitest reports a failure.
pub(super) fn run(source: &Path, suite: Option<&str>) -> anyhow::Result<()> {
    super::verify_source(source)?;
    let metadata = super::cargo_metadata()?;
    let root = metadata.workspace_root.clone();
    let output = metadata
        .target_directory
        .join("xtask/web-assembled-snapshots");
    std::fs::create_dir_all(&output)?;
    let harness_path = output.join("assembled-boot.ts");
    std::fs::write(&harness_path, harness(&root)?)?;
    let setup = output.join("setup.mjs");
    std::fs::write(
        &setup,
        r"// wasm-bindgen ESM wrappers fetch their `.wasm` beside the module; Node's fetch has no
// file: support, so the lane serves those bytes from disk.
import { readFile } from 'node:fs/promises'
import { fileURLToPath } from 'node:url'
const original = globalThis.fetch
globalThis.fetch = async (input, init) => {
  const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url
  const wasm = { headers: { 'content-type': 'application/wasm' } }
  if (url.startsWith('file:')) return new Response(await readFile(fileURLToPath(url)), wasm)
  // Vite rewrites `new URL('x.wasm', import.meta.url)` to its dev-server `/@fs/` form under jsdom.
  const served = /^https?:\/\/[^/]+\/@fs(\/.*\.wasm)(\?.*)?$/.exec(url)
  if (served !== null) return new Response(await readFile(decodeURIComponent(served[1])), wasm)
  if (url.endsWith('.wasm')) throw new Error('assembled lane: unexpected wasm fetch ' + url)
  return original(input, init)
}
",
    )?;
    let selected = SUITES
        .iter()
        .filter(|name| suite.is_none_or(|suite| suite.split(',').any(|wanted| wanted == **name)))
        .map(|name| {
            serde_json::to_string(
                &source
                    .join(format!("apps/web/tests/{name}.snapshot.ts"))
                    .to_string_lossy(),
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    anyhow::ensure!(
        !selected.is_empty(),
        "no assembled snapshot suite matches {suite:?}"
    );
    let config = output.join("vitest.config.mjs");
    // The port's ui-primitives wrapper imports shiki, katex, and micromark by bare name; they
    // resolve through the source's ui-primitives package (its pinned dependency links) so
    // package export maps apply.
    std::fs::write(
        &config,
        format!(
            "const HARNESS = {harness};\nconst PRIMITIVES_IMPORTER = {primitives};\nconst portDependencies = {{\n  name: 'seekdeep-port-dependencies', enforce: 'pre',\n  resolveId(id, _importer, options) {{\n    if (!/^(shiki|@shikijs\\/|katex|micromark-util-sanitize-uri)/.test(id)) return null;\n    return this.resolve(id, PRIMITIVES_IMPORTER, {{ ...options, skipSelf: true }});\n  }},\n  // Product identity: the pinned suites name source packages; the port's plugins carry the\n  // renamed identities (the keyless lane's golden rewrite, applied to the test text).\n  transform(code, id) {{\n    if (!id.includes('/apps/web/tests/')) return null;\n    return {{ code: code.replaceAll('@deepseek-ai/dsh-', '@seekdeep-ai/seekdeep-').replaceAll('DeepSeek Harness', 'SeekDeep Harness'), map: null }};\n  }},\n}};\nexport default {{\n  plugins: [portDependencies],\n  resolve: {{ alias: {aliases} }},\n  test: {{ environment: 'jsdom', include: [{include}], setupFiles: [{setup}], fileParallelism: false, maxWorkers: 1, testTimeout: 60000, hookTimeout: 60000 }},\n}};\n",
            harness = serde_json::to_string(&harness_path.to_string_lossy())?,
            primitives = serde_json::to_string(
                &source
                    .join("packages/client/ui-primitives/src/index.ts")
                    .to_string_lossy()
            )?,
            aliases = aliases(&root, source)?,
            include = selected.join(", "),
            setup = serde_json::to_string(&setup.to_string_lossy())?,
        ),
    )?;
    let status = Command::new("node")
        .arg(source.join("node_modules/vitest/vitest.mjs"))
        .args(["run", "--config"])
        .arg(&config)
        .env("SEEKDEEP_SNAPSHOT", "replay")
        .env("DSH_SNAPSHOT", "replay")
        .env("CI", "1")
        .current_dir(source)
        .status()?;
    anyhow::ensure!(status.success(), "assembled snapshot lane failed");
    Ok(())
}
