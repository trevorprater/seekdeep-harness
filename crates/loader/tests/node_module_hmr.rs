//! Real Node cache identity, replacement, rollback, and realm ownership.

use std::path::Path;

use seekdeep_cordis::{Context, ServiceKey};
use seekdeep_loader::{EntryId, HostHmrOutcome, LOADER, PluginCatalog};
use serde_json::{Value, json};

const FIRST: ServiceKey<Value> = ServiceKey::new("first");
const SECOND: ServiceKey<Value> = ServiceKey::new("second");
const RELOAD_EVENT: ServiceKey<Value> = ServiceKey::new("reloadEvent");
const CHANGE_EVENT: ServiceKey<Value> = ServiceKey::new("changeEvent");

fn dependency(value: &str, fail: bool) -> String {
    format!(
        "import {{ appendFileSync }} from 'node:fs';\nimport {{ fileURLToPath }} from 'node:url';\nimport state from './state.cjs';\nappendFileSync(fileURLToPath(new URL('evaluations.txt', import.meta.url)), 'e');\nexport const shared = state;\nexport const value = {value:?};\nexport const fail = {fail};\n"
    )
}

fn plugin() -> &'static str {
    concat!(
        "import { shared, value, fail } from './dep.mjs';\n",
        "import { createRequire } from 'node:module';\n",
        "import { createHash } from 'node:crypto';\n",
        "import { appendFileSync } from 'node:fs';\n",
        "import { fileURLToPath } from 'node:url';\n",
        "const require = createRequire(import.meta.url);\n",
        "const same = shared === require('./state.cjs');\n",
        "export function apply(ctx, config) {\n",
        "  if (fail && config.service === 'second') throw new Error('second apply failed');\n",
        "  shared.activations += 1;\n",
        "  ctx.provide(config.service, { value, same, activations: shared.activations, digest: createHash('sha256').update('abc').digest('hex'), pid: process.pid });\n",
        "  ctx.effect(() => () => appendFileSync(fileURLToPath(new URL('disposals.txt', import.meta.url)), config.service + ','));\n",
        "}\n",
    )
}

fn write_fixture(root: &Path) -> anyhow::Result<()> {
    std::fs::write(root.join("first.mjs"), plugin())?;
    std::fs::write(root.join("second.mjs"), plugin())?;
    std::fs::write(
        root.join("state.cjs"),
        "module.exports = { activations: 0 };\n",
    )?;
    std::fs::write(root.join("dep.mjs"), dependency("old", false))?;
    std::fs::write(
        root.join("observer.mjs"),
        concat!(
            "export function apply(ctx) {\n",
            "  let remove;\n",
            "  ctx.on('hmr/reload', async reloads => {\n",
            "    if (!(reloads instanceof Map)) throw new Error('reload payload is not a Map');\n",
            "    if (remove) await remove();\n",
            "    remove = ctx.provide('reloadEvent', [...reloads].map(([plugin, reload]) => ({ callable: typeof plugin.apply === 'function', filename: reload.filename, fibers: reload.runtime.fibers.map(fiber => ({id: fiber.entry.id, config: fiber._config, same: fiber.config === fiber._config})) })));\n",
            "  });\n",
            "  ctx.on('hmr/change', url => ctx.provide('changeEvent', url));\n",
            "}\n",
        ),
    )?;
    Ok(())
}

const YAML: &str = "- id: first\n  name: ./first.mjs\n  config: { service: first }\n- id: second\n  name: ./second.mjs\n  config: { service: second }\n- id: observer\n  name: ./observer.mjs\n";

async fn wait_for(mut predicate: impl FnMut() -> bool) -> anyhow::Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !predicate() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await?;
    Ok(())
}

async fn assert_reload_event(context: &Context, root: &Path) -> anyhow::Result<()> {
    wait_for(|| context.get(RELOAD_EVENT).is_some()).await?;
    let reload_event = context.get(RELOAD_EVENT).unwrap();
    assert_eq!(reload_event.as_array().map(Vec::len), Some(2));
    for (index, name) in ["first", "second"].into_iter().enumerate() {
        assert_eq!(reload_event[index]["callable"], true);
        assert_eq!(
            reload_event[index]["filename"],
            url::Url::from_file_path(root.join(format!("{name}.mjs")).canonicalize()?)
                .unwrap()
                .to_string()
        );
        assert_eq!(
            reload_event[index]["fibers"],
            json!([{"id":name,"config":{"service":name},"same":true}])
        );
    }
    Ok(())
}

fn assert_initial_identity(first: &Value, second: &Value, root: &Path) -> anyhow::Result<()> {
    assert_eq!(first["same"], true);
    assert_eq!(second["same"], true);
    assert_eq!(first["pid"], second["pid"]);
    assert_eq!(first["activations"], 1);
    assert_eq!(second["activations"], 2);
    assert_eq!(
        first["digest"],
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(std::fs::read_to_string(root.join("evaluations.txt"))?, "e");
    Ok(())
}

#[tokio::test]
async fn esm_and_commonjs_share_native_identity_across_reload_and_failed_candidates()
-> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path();
    write_fixture(root)?;
    let catalog = PluginCatalog::new();
    let context = Context::new();
    let composition = catalog
        .load_yaml_at(&context, YAML, root.join("cordis.yml"))
        .await?;
    let first = context.get(FIRST).expect("first plugin");
    let second = context.get(SECOND).expect("second plugin");
    assert_initial_identity(&first, &second, root)?;

    std::fs::write(
        root.join("dep.mjs"),
        "throw new Error('candidate import failed'); export const shared = {}; export const value = ''; export const fail = false;\n",
    )?;
    let error = composition
        .reload_module(root.join("dep.mjs"))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("candidate import failed"));
    assert_eq!(context.get(FIRST).as_deref(), Some(first.as_ref()));
    assert_eq!(context.get(SECOND).as_deref(), Some(second.as_ref()));
    assert!(!root.join("disposals.txt").exists());

    std::fs::write(root.join("dep.mjs"), dependency("new", false))?;
    assert_eq!(
        composition.reload_module(root.join("dep.mjs")).await?,
        HostHmrOutcome::Reloaded(vec![EntryId::new("first")?, EntryId::new("second")?])
    );
    context.get(LOADER).unwrap().wait().await?;
    assert_eq!(context.get(FIRST).unwrap()["value"], "new");
    assert_eq!(context.get(SECOND).unwrap()["value"], "new");
    assert_eq!(context.get(FIRST).unwrap()["activations"], 3);
    assert_eq!(context.get(SECOND).unwrap()["activations"], 4);
    assert_eq!(std::fs::read_to_string(root.join("evaluations.txt"))?, "ee");
    assert_reload_event(&context, root).await?;
    let untracked = root.join("untracked.mjs");
    std::fs::write(&untracked, "export {};\n")?;
    assert_eq!(
        composition.reload_module(&untracked).await?,
        HostHmrOutcome::Untracked
    );
    wait_for(|| context.get(CHANGE_EVENT).is_some()).await?;
    assert_eq!(
        context.get(CHANGE_EVENT).unwrap().as_str(),
        Some(
            url::Url::from_file_path(untracked.canonicalize()?)
                .unwrap()
                .as_str()
        )
    );

    std::fs::write(root.join("dep.mjs"), dependency("rejected", true))?;
    assert_eq!(
        composition.reload_module(root.join("dep.mjs")).await?,
        HostHmrOutcome::Reloaded(vec![EntryId::new("first")?, EntryId::new("second")?])
    );
    assert!(
        context
            .get(LOADER)
            .unwrap()
            .wait()
            .await
            .unwrap_err()
            .to_string()
            .contains("second apply failed")
    );
    assert_eq!(context.get(FIRST).unwrap()["value"], "rejected");
    assert!(context.get(SECOND).is_none());
    assert_eq!(
        composition
            .fibers()
            .iter()
            .find(|fiber| fiber.entry_id().as_deref() == Some("second"))
            .unwrap()
            .fiber()
            .state(),
        seekdeep_cordis::FiberState::Failed
    );
    assert_eq!(context.get(FIRST).unwrap()["pid"], first["pid"]);
    assert_eq!(
        std::fs::read_to_string(root.join("disposals.txt"))?,
        "first,second,first,second,"
    );

    std::fs::write(
        root.join("state.cjs"),
        "module.exports = { activations: 100 };\n",
    )?;
    std::fs::write(root.join("dep.mjs"), dependency("final", false))?;
    composition.reload_module(root.join("state.cjs")).await?;
    context.get(LOADER).unwrap().wait().await?;
    assert_eq!(context.get(FIRST).unwrap()["value"], "final");
    assert_eq!(context.get(SECOND).unwrap()["value"], "final");
    assert_eq!(context.get(FIRST).unwrap()["same"], true);
    assert_eq!(context.get(SECOND).unwrap()["same"], true);
    assert_eq!(context.get(FIRST).unwrap()["activations"], 101);
    assert_eq!(context.get(SECOND).unwrap()["activations"], 102);
    composition.dispose().await?;
    assert!(context.get(FIRST).is_none());
    assert!(context.get(SECOND).is_none());
    Ok(())
}

#[tokio::test]
async fn catalog_realms_are_isolated_and_effect_callbacks_release_on_dispose() -> anyhow::Result<()>
{
    let temporary = tempfile::tempdir()?;
    let path = temporary.path().join("plugin.mjs");
    std::fs::write(
        &path,
        concat!(
            "globalThis.catalogLoads = (globalThis.catalogLoads || 0) + 1;\n",
            "export function apply(ctx) {\n",
            "  ctx.provide('first', { loads: globalThis.catalogLoads, pid: process.pid });\n",
            "  ctx.on('native-test', value => { ctx.provide('second', value); });\n",
            "}\n",
        ),
    )?;
    let mut catalogs = Vec::new();
    let mut compositions = Vec::new();
    let mut contexts = Vec::new();
    for _ in 0..2 {
        let catalog = PluginCatalog::new();
        let context = Context::new();
        let composition = catalog
            .load_yaml_at(
                &context,
                "- id: plugin\n  name: ./plugin.mjs\n",
                temporary.path().join("cordis.yml"),
            )
            .await?;
        contexts.push(context);
        compositions.push(composition);
        catalogs.push(catalog);
    }
    assert_eq!(contexts[0].get(FIRST).unwrap()["loads"], 1);
    assert_eq!(contexts[1].get(FIRST).unwrap()["loads"], 1);
    assert_ne!(
        contexts[0].get(FIRST).unwrap()["pid"],
        contexts[1].get(FIRST).unwrap()["pid"]
    );
    #[cfg(unix)]
    let processes = contexts
        .iter()
        .map(|context| context.get(FIRST).unwrap()["pid"].as_u64().unwrap())
        .collect::<Vec<_>>();
    let payload = json!({"received":true});
    contexts[0]
        .events()
        .parallel(
            &contexts[0],
            "native-test",
            &seekdeep_cordis::EventArgs::one(payload.clone()),
        )
        .await?;
    assert_eq!(contexts[0].get(SECOND).as_deref(), Some(&payload));
    for composition in compositions.drain(..) {
        composition.dispose().await?;
    }
    contexts[0]
        .events()
        .parallel(
            &contexts[0],
            "native-test",
            &seekdeep_cordis::EventArgs::one(json!({"received":false})),
        )
        .await?;
    assert!(contexts[0].get(SECOND).is_none());
    drop(compositions);
    drop(catalogs);
    #[cfg(unix)]
    for process in processes {
        assert!(
            !std::process::Command::new("kill")
                .args(["-0", &process.to_string()])
                .stderr(std::process::Stdio::null())
                .status()?
                .success(),
            "catalog still owns live Node process {process} after disposal"
        );
    }
    Ok(())
}

#[tokio::test]
async fn realm_exit_rejects_pending_and_future_requests_and_withdraws_effects() -> anyhow::Result<()>
{
    let temporary = tempfile::tempdir()?;
    let module = temporary.path().join("exit.mjs");
    std::fs::write(
        &module,
        "export function apply(ctx) { ctx.provide('first', process.pid); ctx.on('exit-node-realm', () => { process.exit(23); }); }\n",
    )?;
    let context = Context::new();
    let composition = PluginCatalog::new()
        .load_yaml_at(
            &context,
            "- id: exits\n  name: ./exit.mjs\n",
            temporary.path().join("cordis.yml"),
        )
        .await?;
    let pid = context.get(FIRST).unwrap().as_u64().unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        context.events().parallel(
            &context,
            "exit-node-realm",
            &seekdeep_cordis::EventArgs::new(),
        ),
    )
    .await?;
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Node plugin realm closed")
    );
    let later = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        composition.reload_module(&module),
    )
    .await?;
    assert!(
        later
            .unwrap_err()
            .to_string()
            .contains("Node plugin realm closed")
    );
    wait_for(|| context.get(FIRST).is_none()).await?;
    #[cfg(unix)]
    wait_for(|| {
        !std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    })
    .await?;
    let _ = composition.dispose().await;
    Ok(())
}

#[tokio::test]
async fn package_conditions_subpath_aliases_and_excluded_reloads_use_native_identity()
-> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let installed = temporary.path().join("installed");
    let package = installed.join("node_modules/fixture-plugin");
    let configs = temporary.path().join("configs");
    std::fs::create_dir_all(&package)?;
    std::fs::create_dir_all(&configs)?;
    std::fs::write(
        package.join("package.json"),
        serde_json::to_vec(&json!({
            "name":"fixture-plugin",
            "type":"module",
            "exports":{
                ".":{"node":"./native.mjs","default":"./fallback.mjs"},
                "./alias":"./native.mjs",
                "./private":null
            }
        }))?,
    )?;
    std::fs::write(
        package.join("fallback.mjs"),
        "throw new Error('default export must not override the node condition');\n",
    )?;
    let native = package.join("native.mjs");
    let source = |generation: &str| {
        format!(
            "let activations = 0; export function apply(ctx, config) {{ ctx.provide(config.service, {{generation: {generation:?}, activations: ++activations}}); }}\n"
        )
    };
    std::fs::write(&native, source("old"))?;
    let catalog = PluginCatalog::new().with_bare_module_base(installed.join("host.cjs"));
    let rejected = catalog
        .load_yaml_at(
            &Context::new(),
            "- id: private\n  name: fixture-plugin/private\n",
            configs.join("cordis.yml"),
        )
        .await
        .unwrap_err();
    assert!(rejected.to_string().contains("not defined by \"exports\""));
    let context = Context::new();
    let composition = catalog
        .load_yaml_at(
            &context,
            "- id: first\n  name: fixture-plugin\n  config: { service: first }\n- id: second\n  name: fixture-plugin/alias\n  config: { service: second }\n",
            configs.join("cordis.yml"),
        )
        .await?;
    assert_eq!(
        context.get(FIRST).unwrap().as_ref(),
        &json!({"generation":"old","activations":1})
    );
    assert_eq!(
        context.get(SECOND).unwrap().as_ref(),
        &json!({"generation":"old","activations":2})
    );
    std::fs::write(&native, source("new"))?;
    assert_eq!(
        composition.reload_module(&native).await?,
        HostHmrOutcome::Reloaded(Vec::new())
    );
    context.get(LOADER).unwrap().wait().await?;
    assert_eq!(
        context.get(FIRST).unwrap().as_ref(),
        &json!({"generation":"old","activations":1})
    );
    assert_eq!(
        context.get(SECOND).unwrap().as_ref(),
        &json!({"generation":"old","activations":2})
    );
    composition.dispose().await?;
    Ok(())
}

#[tokio::test]
async fn programmatic_file_fibers_join_reload_batches_and_retain_their_owners() -> anyhow::Result<()>
{
    let temporary = tempfile::tempdir()?;
    let root = temporary.path();
    write_fixture(root)?;
    let context = Context::new();
    let composition = PluginCatalog::new()
        .load_yaml_at(
            &context,
            "- id: first\n  name: ./first.mjs\n  config: { service: first }\n",
            root.join("cordis.yml"),
        )
        .await?;
    let loader = context.get(LOADER).unwrap();
    loader
        .create_programmatic_entry(serde_json::from_value(json!({
            "id":"second", "name":"./second.mjs", "config":{"service":"second"}
        }))?)
        .await?;
    assert_eq!(context.get(FIRST).unwrap()["value"], "old");
    assert_eq!(context.get(SECOND).unwrap()["value"], "old");
    let observation = context.events().on(
        &context,
        "internal/plugin",
        {
            let loader = loader.clone();
            move |_, _| {
                let loader = loader.clone();
                Box::pin(async move {
                    assert_eq!(loader.entries()?.len(), 2);
                    Ok(seekdeep_cordis::EventReply::Undefined)
                })
            }
        },
        seekdeep_cordis::EventOptions::default(),
    )?;
    std::fs::write(root.join("dep.mjs"), dependency("new", false))?;
    assert_eq!(
        composition.reload_module(root.join("dep.mjs")).await?,
        HostHmrOutcome::Reloaded(vec![EntryId::new("first")?, EntryId::new("second")?])
    );
    loader.wait().await?;
    assert_eq!(context.get(FIRST).unwrap()["value"], "new");
    assert_eq!(context.get(SECOND).unwrap()["value"], "new");
    observation.dispose().await?;
    assert!(
        loader
            .remove_programmatic_entry_if_present(&EntryId::new("second")?)
            .await?
    );
    assert!(context.get(SECOND).is_none());
    assert!(context.get(FIRST).is_some());
    composition.dispose().await?;
    Ok(())
}

#[tokio::test]
async fn equal_entry_ids_in_separate_includes_reload_their_exact_fibers() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let root = temporary.path();
    write_fixture(root)?;
    for service in ["first", "second"] {
        std::fs::write(
            root.join(format!("{service}.yml")),
            format!("- id: shared\n  name: ./first.mjs\n  config: {{ service: {service} }}\n"),
        )?;
    }
    let context = Context::new();
    let composition = PluginCatalog::new()
        .load_yaml_at(
            &context,
            "- id: first-include\n  name: cordis:include\n  config: { path: ./first.yml }\n- id: second-include\n  name: cordis:include\n  config: { path: ./second.yml }\n",
            root.join("cordis.yml"),
        )
        .await?;
    assert_eq!(context.get(FIRST).unwrap()["value"], "old");
    assert_eq!(context.get(SECOND).unwrap()["value"], "old");
    std::fs::write(root.join("dep.mjs"), dependency("new", false))?;
    assert_eq!(
        composition.reload_module(root.join("dep.mjs")).await?,
        HostHmrOutcome::Reloaded(vec![EntryId::new("shared")?, EntryId::new("shared")?])
    );
    context.get(LOADER).unwrap().wait().await?;
    assert_eq!(context.get(FIRST).unwrap()["value"], "new");
    assert_eq!(context.get(SECOND).unwrap()["value"], "new");
    assert_eq!(context.get(FIRST).unwrap()["activations"], 3);
    assert_eq!(context.get(SECOND).unwrap()["activations"], 4);
    assert_eq!(
        std::fs::read_to_string(root.join("disposals.txt"))?,
        "first,second,"
    );
    composition.dispose().await?;
    assert!(context.get(FIRST).is_none());
    assert!(context.get(SECOND).is_none());
    Ok(())
}
