//! Built Web application verification over workspace package exports.

use std::{path::Path, process::Command};

use seekdeep_core::{
    chunk_rows::decode_storage_record,
    session::{SessionEvent, SessionHeader, SessionId},
    session_store::SessionStore,
};
use seekdeep_session_persistence::SessionPersistence as _;
use seekdeep_session_persistence_jsonl::{
    JsonlCompression, JsonlConfig, JsonlSessionPersistence, header_line, log_path,
    zstd::compress_zstd_frame,
};

pub(super) fn run(source: &Path) -> anyhow::Result<()> {
    super::verify_source(source)?;
    let metadata = super::cargo_metadata()?;
    let temporary = tempfile::tempdir()?;
    let home = temporary.path().canonicalize()?;
    let workspace = home.join("workspace");
    std::fs::create_dir(&workspace)?;
    std::fs::write(
        home.join("cordis.patch.yml"),
        "- id: session-query-sqlite\n  config:\n    path: ':memory:'\n    openAt: first-search\n",
    )?;
    let id = "web-assembled-seed";
    tokio::runtime::Runtime::new()?.block_on(seed(source, &home, &workspace, id))?;
    let output = metadata.target_directory.join("xtask/web-assembled");
    std::fs::create_dir_all(&output)?;
    let driver = output.join("browser.mjs");
    std::fs::write(&driver, super::web_assembled_driver::DRIVER)?;
    let status = Command::new("node")
        .arg(driver)
        .arg(source)
        .arg(metadata.target_directory.join("debug/seekdeep"))
        .arg(&home)
        .arg(&workspace)
        .arg(&output)
        .arg(id)
        .current_dir(&metadata.workspace_root)
        .status()?;
    anyhow::ensure!(status.success(), "assembled Web browser path failed");
    Ok(())
}

async fn seed(source: &Path, home: &Path, workspace: &Path, id: &str) -> anyhow::Result<()> {
    let fixture = std::fs::read_to_string(
        source.join("apps/web/tests/snapshots/navigation-panes/seed.jsonl"),
    )?
    .replace("{{sessionId}}", id)
    .replace("{{cwd}}/workspace", &workspace.to_string_lossy())
    .replace("{{cwd}}", &workspace.to_string_lossy());
    let mut events: Vec<SessionEvent> = Vec::new();
    for line in fixture.lines().skip(1) {
        for value in decode_storage_record(serde_json::from_str(line)?)? {
            events.push(serde_json::from_value(value)?);
        }
    }
    anyhow::ensure!(
        events
            .last()
            .is_some_and(|event| event.event_type == "turn/end"),
        "assembled Web seed must contain a closed recorded turn"
    );
    let context = seekdeep_cordis::Context::new();
    let persistence = JsonlSessionPersistence::new(
        SessionStore::install(&context)?,
        JsonlConfig::new(home.join("sessions")),
    )?;
    let mut header = SessionHeader::new(SessionId::new(id));
    header.cwd = Some(workspace.to_string_lossy().into_owned());
    header.delegation_depth = Some(0);
    let result = async {
        let path = log_path(
            &home.join("sessions"),
            header.cwd.as_deref(),
            &header.id,
            JsonlCompression::Zstd,
        )?;
        std::fs::create_dir_all(path.parent().expect("session artifact has a parent"))?;
        let (_, body) = fixture
            .split_once('\n')
            .ok_or_else(|| anyhow::anyhow!("source session fixture has no body"))?;
        let mut bytes = compress_zstd_frame(format!("{}\n", header_line(&header)?).as_bytes())?;
        bytes.extend(compress_zstd_frame(body.as_bytes())?);
        std::fs::write(path, bytes)?;
        persistence.inspect(&header.id, None).await?;
        Ok(())
    }
    .await;
    context.root_fiber().dispose().await?;
    result
}

pub(super) fn build() -> anyhow::Result<()> {
    let metadata = super::cargo_metadata()?;
    let root = &metadata.workspace_root;
    for (package, artifact, module, output) in [
        (
            "seekdeep-cordis",
            "seekdeep_cordis",
            "@seekdeep-ai/cordis",
            "vendor/cordis/lib",
        ),
        (
            "seekdeep-client-loader",
            "seekdeep_client_loader",
            "@seekdeep-ai/cordis-plugin-loader",
            "vendor/loader/lib",
        ),
        (
            "seekdeep-client-modules",
            "seekdeep_client_modules",
            "@seekdeep-ai/seekdeep-client-modules",
            "packages/client/modules/lib",
        ),
        (
            "seekdeep-client-ui-slots",
            "seekdeep_client_ui_slots",
            "@seekdeep-ai/seekdeep-client-ui-slots",
            "packages/client/ui-slots/lib",
        ),
        (
            "seekdeep-client-web-react",
            "seekdeep_client_web_react",
            "@seekdeep-ai/seekdeep-client-web-react",
            "packages/client/web-react/lib",
        ),
        (
            "seekdeep-client-ui-primitives",
            "seekdeep_client_ui_primitives",
            "@seekdeep-ai/seekdeep-client-ui-primitives",
            "packages/client/ui-primitives/lib",
        ),
        (
            "seekdeep-client-ui-attachment",
            "seekdeep_client_ui_attachment",
            "@seekdeep-ai/seekdeep-client-ui-attachment",
            "packages/client/ui-attachment/lib",
        ),
        (
            "seekdeep-client-schema-form",
            "seekdeep_client_schema_form",
            "@seekdeep-ai/seekdeep-client-schema-form",
            "packages/client/schema-form/lib",
        ),
        (
            "seekdeep-client-web",
            "seekdeep_client_web",
            "@seekdeep-ai/seekdeep-client-web",
            "packages/client/web/lib",
        ),
    ] {
        super::wasm_package_once(package, artifact, module, &root.join(output))?;
    }
    super::write_web_frontend(root)?;
    let directory = metadata.target_directory.join("xtask/web-assembled");
    std::fs::create_dir_all(&directory)?;
    let driver = directory.join("build.mjs");
    std::fs::write(&driver, BUILD)?;
    let status = Command::new("node")
        .arg(driver)
        .current_dir(root)
        .status()?;
    anyhow::ensure!(status.success(), "assembled Web frontend build failed");
    Ok(())
}

const BUILD: &str = r#"import { readFile, mkdir, symlink, realpath } from 'node:fs/promises';
import { execFileSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';
import { pathToFileURL } from 'node:url';
const root = process.cwd();
const prefix = join(root, 'support/browser-dependencies');
const require = createRequire(join(prefix, 'package.json'));
const dependencies = JSON.parse(await readFile(join(prefix, 'package.json'), 'utf8'));
const external = new Set(Object.keys(dependencies.dependencies));
const viteUrl = pathToFileURL(join(prefix, 'node_modules/vite/dist/node/index.js')).href;
const reactUrl = pathToFileURL(join(prefix, 'node_modules/@vitejs/plugin-react/dist/index.js')).href;
const { build } = await import(viteUrl);
const configSource = (await readFile(join(root, 'apps/web/generated/vite.config.mjs'), 'utf8'))
  .replace("from '@vitejs/plugin-react'", `from ${JSON.stringify(reactUrl)}`)
  .replace("from 'vite'", `from ${JSON.stringify(viteUrl)}`);
const { default: config } = await import('data:text/javascript;base64,' + Buffer.from(configSource).toString('base64'));
const packages = new Map();
for (const path of execFileSync('git', ['ls-files', 'packages/**/package.json', 'vendor/*/package.json'], { cwd: root, encoding: 'utf8' }).trim().split('\n')) {
  const manifest = JSON.parse(await readFile(join(root, path), 'utf8'));
  if (!manifest.name?.startsWith('@seekdeep-ai/')) continue;
  const directory = dirname(join(root, path));
  const link = join(prefix, 'node_modules', manifest.name);
  await mkdir(dirname(link), { recursive: true });
  try { await symlink(directory, link, 'dir'); }
  catch (error) {
    if (error.code !== 'EEXIST' || await realpath(link) !== await realpath(directory)) throw error;
  }
  packages.set(manifest.name, directory);
}
const packageName = name => name.startsWith('@') ? name.split('/').slice(0, 2).join('/') : name.split('/')[0];
const styles = await readFile(join(root, 'packages/client/web/lib/base.css'), 'utf8');
const styleAliases = Object.fromEntries([...styles.matchAll(/@import\s+['"]([^'"]+)['"]/g)]
  .map(([, id]) => [id, require.resolve(id)]));
const resolver = {
  name: 'seekdeep-workspace-browser-exports',
  enforce: 'pre',
  async resolveId(id) {
    if (id.startsWith('.') || id.startsWith('/') || id.startsWith('\0')) return;
    const name = packageName(id);
    if (packages.has(name) || external.has(name)) return this.resolve(id, join(prefix, 'package.json'), { skipSelf: true });
  },
};
await build({ ...config, configFile: false, root: join(root, 'apps/web'), resolve: {
  alias: { ...styleAliases, react: join(prefix, 'node_modules/react'), 'react-dom': join(prefix, 'node_modules/react-dom') },
}, plugins: [...config.plugins, resolver] });
"#;
