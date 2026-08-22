//! Executable composition and patch lifecycle parity.

use std::sync::Arc;

use parking_lot::Mutex;
use seekdeep_cordis::{Context, Plugin, fiber::EffectHandle};
use seekdeep_loader::{
    ConfigTree, Entry, EntryId, EntryParent, EntryUpdate, ExpressionEnvironment, HostHmrOutcome,
    LOADER, LoaderError, Patch, PluginCatalog, PluginSpecifier,
};
use serde_json::json;

const PHASE: seekdeep_cordis::ServiceKey<serde_json::Value> =
    seekdeep_cordis::ServiceKey::new("phase");
const OBSERVED: seekdeep_cordis::ServiceKey<serde_json::Value> =
    seekdeep_cordis::ServiceKey::new("observed");
const JS_VALUE: seekdeep_cordis::ServiceKey<serde_json::Value> =
    seekdeep_cordis::ServiceKey::new("jsValue");
const HMR_VALUE: seekdeep_cordis::ServiceKey<serde_json::Value> =
    seekdeep_cordis::ServiceKey::new("hmrValue");
const HMR_A: seekdeep_cordis::ServiceKey<serde_json::Value> =
    seekdeep_cordis::ServiceKey::new("hmrA");
const HMR_B: seekdeep_cordis::ServiceKey<serde_json::Value> =
    seekdeep_cordis::ServiceKey::new("hmrB");

fn recording_plugin(name: &'static str, events: Arc<Mutex<Vec<String>>>) -> Plugin {
    Plugin::new(name, std::iter::empty::<&str>(), move |context, config| {
        let events = events.clone();
        Box::pin(async move {
            events.lock().push(format!("start:{name}:{config}"));
            context.own(EffectHandle::synchronous(name, move || {
                events.lock().push(format!("stop:{name}"));
                Ok(())
            }))?;
            Ok(())
        })
    })
}

fn deterministic_expressions() -> ExpressionEnvironment {
    ExpressionEnvironment::new(
        std::collections::BTreeMap::from([
            ("SEEKDEEP_TEST_VALUE".to_owned(), "from-env".to_owned()),
            ("SEEKDEEP_PRESENT".to_owned(), "1".to_owned()),
        ]),
        std::path::PathBuf::from("/workspace"),
        std::path::PathBuf::from("/bin/seekdeep"),
        "linux",
        "v22.0.0",
        std::path::PathBuf::from("/state/seekdeep"),
    )
}

#[tokio::test]
async fn javascript_expressions_use_an_injected_process_facade_and_preserve_raw_config() {
    let catalog = PluginCatalog::new().with_expression_environment(deterministic_expressions());
    catalog
        .register_named(
            "capture",
            Plugin::new("capture", std::iter::empty::<&str>(), |context, config| {
                Box::pin(async move {
                    context.provide(OBSERVED, Arc::new(config))?;
                    Ok(())
                })
            }),
        )
        .unwrap();
    let context = Context::new();
    let composition = catalog
        .load_yaml(
            &context,
            concat!(
                "- id: skipped\n",
                "  name: capture\n",
                "  disabled: !!js process.platform === 'linux'\n",
                "- id: capture\n",
                "  name: capture\n",
                "  config:\n",
                "    value: !!js process.env.SEEKDEEP_TEST_VALUE\n",
                "    cwd: !!js process.cwd()\n",
                "    executable: !!js process.execPath\n",
                "    home: !!js seekdeepHomePath('sessions')\n",
            ),
        )
        .await
        .unwrap();
    assert_eq!(composition.fibers().len(), 1);
    assert_eq!(
        context.get(OBSERVED).as_deref(),
        Some(&json!({
            "value": "from-env",
            "cwd": "/workspace",
            "executable": "/bin/seekdeep",
            "home": "/state/seekdeep/sessions",
        }))
    );
    assert_eq!(
        composition.fibers()[0].config()["value"],
        json!({ "__jsExpr": "process.env.SEEKDEEP_TEST_VALUE" })
    );
    composition.dispose().await.unwrap();
}

#[tokio::test]
async fn file_backed_expression_scope_exposes_base_url_and_node_url_conversion() {
    let catalog = PluginCatalog::new().with_expression_environment(deterministic_expressions());
    catalog
        .register_named(
            "capture",
            Plugin::new("capture", std::iter::empty::<&str>(), |context, config| {
                Box::pin(async move {
                    context.provide(OBSERVED, Arc::new(config))?;
                    Ok(())
                })
            }),
        )
        .unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("cordis.yml");
    let source = concat!(
        "- id: capture\n",
        "  name: capture\n",
        "  config:\n",
        "    skills: !!js \"process.getBuiltinModule('node:url').fileURLToPath(new URL('skills/', baseUrl))\"\n",
    );
    let context = Context::new();
    let composition = catalog.load_yaml_at(&context, source, &path).await.unwrap();
    assert_eq!(
        context.get(OBSERVED).expect("capture")["skills"],
        format!("{}/skills/", temporary.path().display())
    );
    composition.dispose().await.unwrap();
}

#[tokio::test]
async fn relative_javascript_plugin_runs_in_a_persistent_rust_owned_module_realm() {
    let temporary = tempfile::tempdir().unwrap();
    let config_path = temporary.path().join("cordis.yml");
    std::fs::write(
        temporary.path().join("provider.mjs"),
        concat!(
            "globalThis.starts ??= 0;\n",
            "globalThis.disposals ??= 0;\n",
            "export const name = 'js-provider';\n",
            "export const inject = [];\n",
            "export async function apply(ctx, config) {\n",
            "  await Promise.resolve();\n",
            "  globalThis.starts += 1;\n",
            "  ctx.provide('jsValue', { configured: config.value, generation: globalThis.starts, disposals: globalThis.disposals });\n",
            "  ctx.effect(() => () => { globalThis.disposals += 1; });\n",
            "}\n",
        ),
    )
    .unwrap();
    let catalog = PluginCatalog::new();
    catalog
        .register_named(
            "consumer",
            Plugin::new("consumer", ["jsValue"], |context, _| {
                Box::pin(async move {
                    context.provide(OBSERVED, context.get(JS_VALUE).expect("js value"))?;
                    Ok(())
                })
            }),
        )
        .unwrap();
    let context = Context::new();
    let composition = catalog
        .load_yaml_at(
            &context,
            concat!(
                "- id: consumer\n",
                "  name: consumer\n",
                "- id: provider\n",
                "  name: ./provider.mjs\n",
                "  config: { value: first }\n",
            ),
            &config_path,
        )
        .await
        .unwrap();
    assert_eq!(
        context.get(OBSERVED).as_deref(),
        Some(&json!({ "configured": "first", "generation": 1, "disposals": 0 }))
    );
    let provider = composition
        .fibers()
        .into_iter()
        .find(|fiber| fiber.plugin_name() == "js-provider")
        .unwrap();
    provider.update(json!({ "value": "second" })).await.unwrap();
    let consumer = composition
        .fibers()
        .into_iter()
        .find(|fiber| fiber.plugin_name() == "consumer")
        .unwrap();
    consumer.await_settled().await.unwrap();
    assert_eq!(
        context.get(OBSERVED).as_deref(),
        Some(&json!({ "configured": "second", "generation": 2, "disposals": 1 }))
    );
    composition.dispose().await.unwrap();
}

#[tokio::test]
async fn malformed_javascript_plugin_fails_import_before_any_entry_mounts() {
    let temporary = tempfile::tempdir().unwrap();
    let config_path = temporary.path().join("cordis.yml");
    std::fs::write(
        temporary.path().join("bad.mjs"),
        "export function apply( {\n",
    )
    .unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let catalog = PluginCatalog::new();
    catalog
        .register_named("record", recording_plugin("record", events.clone()))
        .unwrap();
    let context = Context::new();
    let composition = catalog
        .load_yaml_at(&context, "- id: prior\n  name: record\n", &config_path)
        .await
        .unwrap();
    events.lock().clear();
    let error = composition
        .update_yaml(concat!(
            "- id: prior\n",
            "  name: record\n",
            "- id: bad\n",
            "  name: ./bad.mjs\n",
        ))
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("failed to import loader entry bad")
    );
    assert!(
        events.lock().is_empty(),
        "import preflight ran after mounting"
    );
    composition.dispose().await.unwrap();
}

#[tokio::test]
async fn javascript_default_object_exports_preserve_inject_metadata() {
    let temporary = tempfile::tempdir().unwrap();
    let config_path = temporary.path().join("cordis.yml");
    std::fs::write(
        temporary.path().join("consumer.mjs"),
        concat!(
            "export default {\n",
            "  name: 'object-consumer',\n",
            "  inject: ['phase'],\n",
            "  apply(ctx) { ctx.provide('observed', ctx.phase); },\n",
            "};\n",
        ),
    )
    .unwrap();
    let catalog = PluginCatalog::new();
    catalog
        .register_named(
            "provider",
            Plugin::new("provider", std::iter::empty::<&str>(), |context, _| {
                Box::pin(async move {
                    context.provide(PHASE, Arc::new(json!({ "ready": true })))?;
                    Ok(())
                })
            }),
        )
        .unwrap();
    let context = Context::new();
    let composition = catalog
        .load_yaml_at(
            &context,
            concat!(
                "- id: consumer\n",
                "  name: ./consumer.mjs\n",
                "- id: provider\n",
                "  name: provider\n",
            ),
            config_path,
        )
        .await
        .unwrap();
    assert_eq!(
        context.get(OBSERVED).as_deref(),
        Some(&json!({ "ready": true }))
    );
    assert!(
        composition
            .fibers()
            .iter()
            .any(|fiber| fiber.plugin_name() == "object-consumer")
    );
    composition.dispose().await.unwrap();
}

fn hmr_module_source() -> &'static str {
    concat!(
        "import { value, fail } from './dep.mjs';\n",
        "export const name = 'hmr-plugin';\n",
        "export function apply(ctx) {\n",
        "  if (fail) throw new Error('candidate apply failed');\n",
        "  ctx.provide('hmrValue', value);\n",
        "}\n",
    )
}

#[tokio::test]
async fn host_hmr_prepares_dependencies_then_reloads_and_recovers_transactionally() {
    let temporary = tempfile::tempdir().unwrap();
    let config_path = temporary.path().join("cordis.yml");
    let module_path = temporary.path().join("main.mjs");
    let dependency_path = temporary.path().join("dep.mjs");
    let external_path = temporary.path().join("launcher.mjs");
    let untracked_path = temporary.path().join("untracked.mjs");
    std::fs::write(&module_path, hmr_module_source()).unwrap();
    std::fs::write(
        &dependency_path,
        "export const value = 'old'; export const fail = false;\n",
    )
    .unwrap();
    std::fs::write(&external_path, "export {};\n").unwrap();
    std::fs::write(&untracked_path, "export {};\n").unwrap();
    let catalog = PluginCatalog::new().with_hmr_externals([external_path.clone()]);
    let context = Context::new();
    let hmr_events = Arc::new(Mutex::new(Vec::new()));
    for name in ["hmr/reload", "hmr/change"] {
        let hmr_events = hmr_events.clone();
        context
            .events()
            .on(
                &context,
                name,
                move |_, _| {
                    let hmr_events = hmr_events.clone();
                    Box::pin(async move {
                        hmr_events.lock().push(name.to_owned());
                        Ok(seekdeep_cordis::EventReply::Undefined)
                    })
                },
                seekdeep_cordis::EventOptions::default(),
            )
            .unwrap();
    }
    let composition = catalog
        .load_yaml_at(&context, "- id: hmr\n  name: ./main.mjs\n", &config_path)
        .await
        .unwrap();
    assert_eq!(context.get(HMR_VALUE).as_deref(), Some(&json!("old")));
    let old_fiber = composition.fibers()[0].clone();

    std::fs::write(&dependency_path, "export const value = ;\n").unwrap();
    let import = composition
        .reload_module(&dependency_path)
        .await
        .unwrap_err();
    assert!(import.to_string().contains("dep.mjs"));
    assert!(Arc::ptr_eq(&composition.fibers()[0], &old_fiber));
    assert_eq!(context.get(HMR_VALUE).as_deref(), Some(&json!("old")));

    std::fs::write(
        &dependency_path,
        "export const value = 'bad'; export const fail = true;\n",
    )
    .unwrap();
    let apply = composition
        .reload_module(&dependency_path)
        .await
        .unwrap_err();
    assert!(apply.to_string().contains("candidate apply failed"));
    assert_eq!(context.get(HMR_VALUE).as_deref(), Some(&json!("old")));

    std::fs::write(
        &dependency_path,
        "export const value = 'new'; export const fail = false;\n",
    )
    .unwrap();
    assert_eq!(
        composition.reload_module(&dependency_path).await.unwrap(),
        HostHmrOutcome::Reloaded(vec![EntryId::new("hmr").unwrap()])
    );
    assert_eq!(context.get(HMR_VALUE).as_deref(), Some(&json!("new")));
    assert_eq!(
        composition.reload_module(&external_path).await.unwrap(),
        HostHmrOutcome::FullRestart
    );
    assert_eq!(
        composition.reload_module(&untracked_path).await.unwrap(),
        HostHmrOutcome::Untracked
    );
    assert_eq!(&*hmr_events.lock(), &["hmr/reload", "hmr/change"]);
    composition.dispose().await.unwrap();
}

#[tokio::test]
async fn host_hmr_rolls_back_earlier_plugin_replacements_when_a_later_apply_fails() {
    let temporary = tempfile::tempdir().unwrap();
    let config_path = temporary.path().join("cordis.yml");
    let dependency_path = temporary.path().join("dep.mjs");
    std::fs::write(
        temporary.path().join("a.mjs"),
        concat!(
            "import { value } from './dep.mjs';\n",
            "export function apply(ctx) { ctx.provide('hmrA', value); }\n",
        ),
    )
    .unwrap();
    std::fs::write(
        temporary.path().join("b.mjs"),
        concat!(
            "import { value, fail } from './dep.mjs';\n",
            "export function apply(ctx) {\n",
            "  if (fail) throw new Error('later apply failed');\n",
            "  ctx.provide('hmrB', value);\n",
            "}\n",
        ),
    )
    .unwrap();
    std::fs::write(
        &dependency_path,
        "export const value = 'old'; export const fail = false;\n",
    )
    .unwrap();
    let context = Context::new();
    let composition = PluginCatalog::new()
        .load_yaml_at(
            &context,
            concat!(
                "- id: first\n",
                "  name: ./a.mjs\n",
                "- id: second\n",
                "  name: ./b.mjs\n",
            ),
            config_path,
        )
        .await
        .unwrap();
    assert_eq!(context.get(HMR_A).as_deref(), Some(&json!("old")));
    assert_eq!(context.get(HMR_B).as_deref(), Some(&json!("old")));

    std::fs::write(
        &dependency_path,
        "export const value = 'bad'; export const fail = true;\n",
    )
    .unwrap();
    let error = composition
        .reload_module(&dependency_path)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("later apply failed"));
    assert_eq!(context.get(HMR_A).as_deref(), Some(&json!("old")));
    assert_eq!(context.get(HMR_B).as_deref(), Some(&json!("old")));

    std::fs::write(
        &dependency_path,
        "export const value = 'new'; export const fail = false;\n",
    )
    .unwrap();
    assert_eq!(
        composition.reload_module(&dependency_path).await.unwrap(),
        HostHmrOutcome::Reloaded(vec![
            EntryId::new("first").unwrap(),
            EntryId::new("second").unwrap(),
        ])
    );
    assert_eq!(context.get(HMR_A).as_deref(), Some(&json!("new")));
    assert_eq!(context.get(HMR_B).as_deref(), Some(&json!("new")));
    composition.dispose().await.unwrap();
}

#[tokio::test]
async fn host_hmr_observes_but_does_not_surface_old_generation_disposal_failure() {
    let temporary = tempfile::tempdir().unwrap();
    let config_path = temporary.path().join("cordis.yml");
    let dependency_path = temporary.path().join("dep.mjs");
    std::fs::write(
        temporary.path().join("main.mjs"),
        concat!(
            "import { value } from './dep.mjs';\n",
            "export function apply(ctx) {\n",
            "  ctx.provide('hmrValue', value);\n",
            "  ctx.effect(() => () => { if (value === 'old') throw new Error('old dispose failed'); });\n",
            "}\n",
        ),
    )
    .unwrap();
    std::fs::write(&dependency_path, "export const value = 'old';\n").unwrap();
    let context = Context::new();
    let composition = PluginCatalog::new()
        .load_yaml_at(&context, "- id: hmr\n  name: ./main.mjs\n", config_path)
        .await
        .unwrap();
    std::fs::write(&dependency_path, "export const value = 'new';\n").unwrap();
    assert_eq!(
        composition.reload_module(&dependency_path).await.unwrap(),
        HostHmrOutcome::Reloaded(vec![EntryId::new("hmr").unwrap()])
    );
    assert_eq!(context.get(HMR_VALUE).as_deref(), Some(&json!("new")));
    composition.dispose().await.unwrap();
}

#[tokio::test]
async fn host_hmr_invalidates_commonjs_plugin_generations() {
    let temporary = tempfile::tempdir().unwrap();
    let config_path = temporary.path().join("cordis.yml");
    let module_path = temporary.path().join("plugin.cjs");
    let dependency_path = temporary.path().join("dep.cjs");
    std::fs::write(
        &module_path,
        "const dep = require('./dep.cjs');\nmodule.exports = function(ctx) { ctx.provide('hmrValue', dep.value); };\n",
    )
    .unwrap();
    std::fs::write(&dependency_path, "module.exports = { value: 'old' };\n").unwrap();
    let context = Context::new();
    let composition = PluginCatalog::new()
        .load_yaml_at(&context, "- id: cjs\n  name: ./plugin.cjs\n", config_path)
        .await
        .unwrap();
    assert_eq!(context.get(HMR_VALUE).as_deref(), Some(&json!("old")));
    std::fs::write(&dependency_path, "module.exports = { value: 'new' };\n").unwrap();
    assert_eq!(
        composition.reload_module(&dependency_path).await.unwrap(),
        HostHmrOutcome::Reloaded(vec![EntryId::new("cjs").unwrap()])
    );
    assert_eq!(context.get(HMR_VALUE).as_deref(), Some(&json!("new")));
    composition.dispose().await.unwrap();
}

#[tokio::test]
async fn include_reapplies_patches_from_detached_file_content_and_reconciles_its_subtree() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let catalog = PluginCatalog::new();
    catalog
        .register_named("record", recording_plugin("record", events.clone()))
        .unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root_path = temporary.path().join("cordis.yml");
    let base_path = temporary.path().join("base.yml");
    std::fs::write(
        &base_path,
        "- id: nested\n  name: record\n  config: { value: base }\n",
    )
    .unwrap();
    let root = |value: &str, extra: bool| {
        format!(
            concat!(
                "- id: include\n",
                "  name: cordis:include\n",
                "  config:\n",
                "    path: ./base.yml\n",
                "    patches:\n",
                "      - id: nested\n",
                "        config: {{ value: {} }}\n",
                "{}",
            ),
            value,
            if extra {
                concat!(
                    "      - insert:\n",
                    "          - id: extra\n",
                    "            name: record\n",
                )
            } else {
                ""
            }
        )
    };
    let context = Context::new();
    let composition = catalog
        .load_yaml_at(&context, &root("patched", true), &root_path)
        .await
        .unwrap();
    assert_eq!(composition.fibers().len(), 2);
    assert_eq!(
        &*events.lock(),
        &["start:record:{\"value\":\"patched\"}", "start:record:{}",]
    );

    let loader = context.get(LOADER).unwrap();
    std::fs::write(&base_path, "invalid: [unclosed\n").unwrap();
    let failure = loader.refresh_includes().await.unwrap_err();
    assert!(failure.to_string().contains("failed to parse config file"));
    assert_eq!(events.lock().len(), 2, "invalid content changed the tree");

    std::fs::write(
        &base_path,
        "- id: nested\n  name: record\n  config: { value: edited }\n",
    )
    .unwrap();
    loader.refresh_includes().await.unwrap();
    assert_eq!(events.lock().len(), 2, "patch was not reapplied");

    composition
        .update_yaml(&root("patched-v2", false))
        .await
        .unwrap();
    assert_eq!(
        &*events.lock(),
        &[
            "start:record:{\"value\":\"patched\"}",
            "start:record:{}",
            "stop:record",
            "start:record:{\"value\":\"patched-v2\"}",
            "stop:record",
        ]
    );
    assert_eq!(
        loader
            .entries()
            .unwrap()
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        ["include", "nested"]
    );
    composition.dispose().await.unwrap();
}

#[tokio::test]
async fn include_subtree_uses_a_separate_entry_id_namespace() {
    let catalog = PluginCatalog::new();
    catalog
        .register_named(
            "noop",
            Plugin::new("noop", std::iter::empty::<&str>(), |_, _| {
                Box::pin(async { Ok(()) })
            }),
        )
        .unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root_path = temporary.path().join("cordis.yml");
    std::fs::write(
        temporary.path().join("nested.yml"),
        "- id: duplicate\n  name: noop\n",
    )
    .unwrap();
    let composition = catalog
        .load_yaml_at(
            &Context::new(),
            concat!(
                "- id: duplicate\n",
                "  name: noop\n",
                "- id: include\n",
                "  name: cordis:include\n",
                "  config: { path: ./nested.yml }\n",
            ),
            root_path,
        )
        .await
        .unwrap();
    assert_eq!(composition.fibers().len(), 2);
    composition.dispose().await.unwrap();
}

#[tokio::test]
async fn interpolation_waits_for_injections_and_repeats_after_provider_replacement() {
    let catalog = PluginCatalog::new().with_expression_environment(deterministic_expressions());
    catalog
        .register_named(
            "reader",
            Plugin::new("reader", std::iter::empty::<&str>(), |context, config| {
                Box::pin(async move {
                    context.provide(OBSERVED, Arc::new(config))?;
                    Ok(())
                })
            }),
        )
        .unwrap();
    catalog
        .register_named(
            "provider",
            Plugin::new("provider", std::iter::empty::<&str>(), |context, config| {
                Box::pin(async move {
                    context.provide(PHASE, Arc::new(config))?;
                    Ok(())
                })
            }),
        )
        .unwrap();
    let context = Context::new();
    let composition = catalog
        .load_yaml(
            &context,
            concat!(
                "- id: reader\n",
                "  name: reader\n",
                "  inject: [phase]\n",
                "  config:\n",
                "    value: !!js \"ctx.phase.fail ? (() => { throw new Error('rejected provider') })() : ctx.phase.value\"\n",
                "- id: provider\n",
                "  name: provider\n",
                "  config:\n",
                "    value: first\n",
            ),
        )
        .await
        .unwrap();
    assert_eq!(context.get(OBSERVED).expect("reader")["value"], "first");
    let provider = composition
        .fibers()
        .into_iter()
        .find(|fiber| fiber.plugin_name() == "provider")
        .unwrap();
    provider
        .update_transactional(json!({ "value": "second" }))
        .await
        .unwrap();
    let reader = composition
        .fibers()
        .into_iter()
        .find(|fiber| fiber.plugin_name() == "reader")
        .unwrap();
    reader.await_settled().await.unwrap();
    assert_eq!(context.get(OBSERVED).expect("reader")["value"], "second");

    provider
        .update_transactional(json!({ "fail": true }))
        .await
        .unwrap();
    let failure = reader.await_settled().await.unwrap_err();
    assert!(failure.to_string().contains("rejected provider"));
    assert!(context.get(OBSERVED).is_none());

    provider
        .update_transactional(json!({ "value": "recovered" }))
        .await
        .unwrap();
    reader.await_settled().await.unwrap();
    assert_eq!(context.get(OBSERVED).expect("reader")["value"], "recovered");
    composition.dispose().await.unwrap();
}

#[tokio::test]
async fn disabled_group_stops_descendants_and_invalid_expression_rolls_back_prior_mounts() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let catalog = PluginCatalog::new().with_expression_environment(deterministic_expressions());
    catalog
        .register_named("noop", recording_plugin("noop", events.clone()))
        .unwrap();
    let context = Context::new();
    let disabled = catalog
        .load_yaml(
            &context,
            concat!(
                "- id: group\n",
                "  name: cordis:group\n",
                "  group: true\n",
                "  disabled: !!js process.env.SEEKDEEP_PRESENT\n",
                "  config:\n",
                "    - id: child\n",
                "      name: noop\n",
            ),
        )
        .await
        .unwrap();
    assert!(disabled.fibers().is_empty());
    disabled.dispose().await.unwrap();

    let error = catalog
        .load_yaml(
            &context,
            concat!(
                "- id: prior\n",
                "  name: noop\n",
                "- id: invalid\n",
                "  name: noop\n",
                "  config:\n",
                "    value: !!js JSON.parse('invalid')\n",
            ),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("failed"));
    assert_eq!(&*events.lock(), &["start:noop:{}", "stop:noop"]);
}

#[tokio::test]
async fn grouped_rows_share_one_isolated_service_realm_hidden_from_the_root() {
    let catalog = PluginCatalog::new();
    catalog
        .register_named(
            "provider",
            Plugin::new("provider", std::iter::empty::<&str>(), |context, _| {
                Box::pin(async move {
                    context.provide(PHASE, Arc::new(json!({ "tag": "realm" })))?;
                    Ok(())
                })
            }),
        )
        .unwrap();
    catalog
        .register_named(
            "consumer",
            Plugin::new("consumer", ["phase"], |context, _| {
                Box::pin(async move {
                    let phase = context.get(PHASE).expect("isolated phase");
                    context.provide(OBSERVED, phase)?;
                    Ok(())
                })
            }),
        )
        .unwrap();
    let context = Context::new();
    let composition = catalog
        .load_yaml(
            &context,
            concat!(
                "- id: realm\n",
                "  name: cordis:group\n",
                "  group: true\n",
                "  isolate:\n",
                "    phase: true\n",
                "  config:\n",
                "    - id: provider\n",
                "      name: provider\n",
                "    - id: consumer\n",
                "      name: consumer\n",
            ),
        )
        .await
        .unwrap();
    assert_eq!(context.get(OBSERVED).expect("consumer")["tag"], "realm");
    assert!(context.get(PHASE).is_none());
    composition.dispose().await.unwrap();
}

#[tokio::test]
async fn named_isolation_labels_share_a_realm_across_sibling_entries() {
    let catalog = PluginCatalog::new();
    catalog
        .register_named(
            "provider",
            Plugin::new("provider", std::iter::empty::<&str>(), |context, _| {
                Box::pin(async move {
                    context.provide(PHASE, Arc::new(json!({ "tag": "shared" })))?;
                    Ok(())
                })
            }),
        )
        .unwrap();
    catalog
        .register_named(
            "consumer",
            Plugin::new("consumer", ["phase"], |context, _| {
                Box::pin(async move {
                    context.provide(OBSERVED, context.get(PHASE).expect("named realm"))?;
                    Ok(())
                })
            }),
        )
        .unwrap();
    let context = Context::new();
    let composition = catalog
        .load_yaml(
            &context,
            concat!(
                "- id: provider\n",
                "  name: provider\n",
                "  isolate: { phase: shared }\n",
                "- id: consumer\n",
                "  name: consumer\n",
                "  isolate: { phase: shared }\n",
            ),
        )
        .await
        .unwrap();
    assert_eq!(context.get(OBSERVED).expect("consumer")["tag"], "shared");
    assert!(context.get(PHASE).is_none());
    composition.dispose().await.unwrap();
}

#[tokio::test]
async fn yaml_list_mounts_enabled_entries_and_nested_children_then_disposes_in_reverse() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let catalog = PluginCatalog::new();
    catalog
        .register_named("alpha", recording_plugin("alpha", events.clone()))
        .unwrap();
    catalog
        .register_named("child", recording_plugin("child", events.clone()))
        .unwrap();
    let context = Context::new();
    let composition = catalog
        .load_yaml(
            &context,
            concat!(
                "- id: alpha-entry\n",
                "  name: alpha\n",
                "  config:\n",
                "    value: 7\n",
                "  children:\n",
                "    - id: child-entry\n",
                "      name: child\n",
                "- id: skipped\n",
                "  name: alpha\n",
                "  disabled: true\n",
            ),
        )
        .await
        .unwrap();
    assert_eq!(composition.fibers().len(), 2);
    assert_eq!(
        *events.lock(),
        vec![
            "start:alpha:{\"value\":7}".to_owned(),
            "start:child:{}".to_owned(),
        ]
    );
    composition.dispose().await.unwrap();
    assert_eq!(
        *events.lock(),
        vec![
            "start:alpha:{\"value\":7}".to_owned(),
            "start:child:{}".to_owned(),
            "stop:child".to_owned(),
            "stop:alpha".to_owned(),
        ]
    );
}

#[tokio::test]
async fn unknown_or_failed_later_entry_rolls_back_every_prior_mount() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let catalog = PluginCatalog::new();
    catalog
        .register_named("alpha", recording_plugin("alpha", events.clone()))
        .unwrap();
    let context = Context::new();
    let error = catalog
        .load_yaml(
            &context,
            "- id: first\n  name: alpha\n- id: missing\n  name: absent\n",
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        LoaderError::PluginImport { entry, plugin, .. }
            if entry == "missing" && plugin == "absent"
    ));
    assert_eq!(
        *events.lock(),
        vec!["start:alpha:{}".to_owned(), "stop:alpha".to_owned()]
    );
}

#[test]
fn patch_replaces_nested_rows_wholesale_and_appends_unknown_ids() {
    let mut tree: ConfigTree = serde_json::from_value(json!({
        "entries": [{
            "id": "parent",
            "name": "one",
            "config": { "keep": true },
            "children": [{ "id": "nested", "name": "two" }]
        }]
    }))
    .unwrap();
    let patch: Patch = serde_json::from_value(json!({
        "nested": { "id": "ignored", "name": "replacement", "config": { "next": 1 } },
        "new": { "id": "ignored-too", "name": "three" }
    }))
    .unwrap();
    tree.apply_patch(patch);
    assert_eq!(tree.entries[0].children[0].id.as_str(), "nested");
    assert_eq!(tree.entries[0].children[0].plugin.as_str(), "replacement");
    assert_eq!(tree.entries[0].children[0].config, json!({ "next": 1 }));
    assert!(tree.entries[0].children[0].children.is_empty());
    assert_eq!(tree.entries[1].id.as_str(), "new");
    assert_eq!(tree.entries[1].config, json!({}));
}

#[test]
fn catalog_rejects_duplicate_and_empty_names() {
    let catalog = PluginCatalog::new();
    catalog
        .register_named(
            "same",
            Plugin::new("one", std::iter::empty::<&str>(), |_, _| {
                Box::pin(async { Ok(()) })
            }),
        )
        .unwrap();
    assert!(matches!(
        catalog.register_named(
            "same",
            Plugin::new("two", std::iter::empty::<&str>(), |_, _| {
                Box::pin(async { Ok(()) })
            })
        ),
        Err(LoaderError::DuplicatePlugin(name)) if name == "same"
    ));
    assert!(matches!(
        catalog.register_named(
            " ",
            Plugin::new("empty", std::iter::empty::<&str>(), |_, _| {
                Box::pin(async { Ok(()) })
            })
        ),
        Err(LoaderError::InvalidPluginSpecifier)
    ));
}

#[tokio::test]
async fn entry_ids_are_unique_across_group_boundaries_before_any_mount() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let catalog = PluginCatalog::new();
    catalog
        .register_named("noop", recording_plugin("noop", events.clone()))
        .unwrap();
    let error = catalog
        .load_yaml(
            &Context::new(),
            concat!(
                "- id: duplicate\n",
                "  name: noop\n",
                "- id: group\n",
                "  name: cordis:group\n",
                "  group: true\n",
                "  config:\n",
                "    - id: duplicate\n",
                "      name: noop\n",
            ),
        )
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("duplicate loader entry id: duplicate")
    );
    assert!(events.lock().is_empty());
}

#[tokio::test]
async fn failed_programmatic_move_restores_parent_position_config_and_fiber_identity() {
    let catalog = PluginCatalog::new();
    catalog
        .register_named(
            "movable",
            Plugin::new("movable", std::iter::empty::<&str>(), |_, config| {
                Box::pin(async move {
                    anyhow::ensure!(config["fail"] != true, "candidate config failed");
                    Ok(())
                })
            }),
        )
        .unwrap();
    let context = Context::new();
    let composition = catalog
        .load_yaml(
            &context,
            concat!(
                "- id: group\n",
                "  name: cordis:group\n",
                "  group: true\n",
                "  config: []\n",
                "- id: target\n",
                "  name: movable\n",
                "  config: { fail: false }\n",
            ),
        )
        .await
        .unwrap();
    let previous = composition.fibers()[0].clone();
    let target = EntryId::new("target").unwrap();
    let error = composition
        .update_entry(
            &target,
            EntryUpdate {
                config: Some(json!({ "fail": true })),
                ..EntryUpdate::default()
            },
            EntryParent::Group(EntryId::new("group").unwrap()),
            None,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("candidate config failed"));
    assert_eq!(composition.fibers().len(), 1);
    assert!(Arc::ptr_eq(&composition.fibers()[0], &previous));
    assert_eq!(composition.fibers()[0].config(), json!({ "fail": false }));
    composition.dispose().await.unwrap();
}

#[tokio::test]
async fn context_loader_service_inventories_and_mutates_the_live_tree() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let catalog = PluginCatalog::new();
    catalog
        .register_named("record", recording_plugin("record", events.clone()))
        .unwrap();
    let context = Context::new();
    let composition = catalog
        .load_yaml(
            &context,
            "- id: initial\n  name: record\n  config: { value: initial }\n",
        )
        .await
        .unwrap();
    let loader = context.get(LOADER).expect("root loader service");
    assert_eq!(loader.entries().unwrap()[0].id.as_str(), "initial");

    let mut dynamic = Entry::new(
        EntryId::new("dynamic").unwrap(),
        PluginSpecifier::new("record").unwrap(),
    );
    dynamic.config = json!({ "value": "created" });
    loader
        .create_entry(dynamic, EntryParent::Root, Some(0))
        .await
        .unwrap();
    let snapshot = loader.entries().unwrap();
    assert_eq!(
        snapshot
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        ["dynamic", "initial"]
    );

    let dynamic = EntryId::new("dynamic").unwrap();
    loader
        .update_entry(
            &dynamic,
            EntryUpdate {
                config: Some(json!({ "value": "updated" })),
                ..EntryUpdate::default()
            },
            EntryParent::Keep,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        loader
            .entries()
            .unwrap()
            .iter()
            .find(|entry| entry.id == dynamic)
            .unwrap()
            .config,
        json!({ "value": "updated" })
    );
    loader.remove_entry(&dynamic).await.unwrap();
    assert_eq!(loader.entries().unwrap().len(), 1);
    assert_eq!(
        &*events.lock(),
        &[
            "start:record:{\"value\":\"initial\"}",
            "start:record:{\"value\":\"created\"}",
            "stop:record",
            "start:record:{\"value\":\"updated\"}",
            "stop:record",
        ]
    );
    composition.dispose().await.unwrap();
    assert!(context.get(LOADER).is_none());
    assert!(loader.entries().is_err());
}

#[tokio::test]
async fn live_loader_wait_tracks_the_exact_runtime_update_generation() {
    let catalog = PluginCatalog::new();
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    catalog
        .register_named(
            "blocker",
            Plugin::new("blocker", std::iter::empty::<&str>(), {
                let started = started.clone();
                let release = release.clone();
                move |_, _| {
                    let started = started.clone();
                    let release = release.clone();
                    Box::pin(async move {
                        started.notify_one();
                        release.notified().await;
                        Ok(())
                    })
                }
            }),
        )
        .unwrap();
    let context = Context::new();
    let composition = catalog.load_yaml(&context, "[]\n").await.unwrap();
    let loader = context.get(LOADER).unwrap();
    loader.wait().await.unwrap();

    let creating = tokio::spawn({
        let loader = loader.clone();
        async move {
            loader
                .create_entry(
                    Entry::new(
                        EntryId::new("blocker").unwrap(),
                        PluginSpecifier::new("blocker").unwrap(),
                    ),
                    EntryParent::Root,
                    None,
                )
                .await
        }
    });
    started.notified().await;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(10), loader.wait())
            .await
            .is_err(),
        "loader.wait returned before the admitted generation settled"
    );
    release.notify_one();
    creating.await.unwrap().unwrap();
    loader.wait().await.unwrap();
    composition.dispose().await.unwrap();
}

#[tokio::test]
async fn exact_generation_settlement_waits_for_later_siblings() {
    let catalog = PluginCatalog::new();
    let (settled_sender, settled_receiver) = tokio::sync::oneshot::channel();
    let settled_sender = Arc::new(Mutex::new(Some(settled_sender)));
    catalog
        .register_named(
            "waiter",
            Plugin::new("waiter", ["loader"], move |context, _| {
                let settlement = context.get(LOADER).expect("loader settlement");
                let sender = settled_sender.clone();
                Box::pin(async move {
                    tokio::spawn(async move {
                        let result = settlement.wait().await.map_err(|error| error.to_string());
                        if let Some(sender) = sender.lock().take() {
                            let _ = sender.send(result);
                        }
                    });
                    Ok(())
                })
            }),
        )
        .unwrap();
    let blocker_started = Arc::new(tokio::sync::Notify::new());
    let blocker_release = Arc::new(tokio::sync::Notify::new());
    let started = blocker_started.clone();
    let release = blocker_release.clone();
    catalog
        .register_named(
            "blocker",
            Plugin::new("blocker", std::iter::empty::<&str>(), move |_, _| {
                let started = started.clone();
                let release = release.clone();
                Box::pin(async move {
                    started.notify_one();
                    release.notified().await;
                    Ok(())
                })
            }),
        )
        .unwrap();
    let context = Context::new();
    let loading = tokio::spawn({
        let catalog = catalog.clone();
        let context = context.clone();
        async move {
            catalog
                .load_yaml(
                    &context,
                    "- id: waiter\n  name: waiter\n- id: blocker\n  name: blocker\n",
                )
                .await
        }
    });
    blocker_started.notified().await;
    let mut settled_receiver = settled_receiver;
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(10), &mut settled_receiver)
            .await
            .is_err(),
        "waiter settled before its later sibling"
    );
    blocker_release.notify_one();
    let composition = loading.await.unwrap().unwrap();
    assert_eq!(settled_receiver.await.unwrap(), Ok(()));
    composition.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn failed_generation_wakes_waiters_only_after_rollback() {
    let catalog = PluginCatalog::new();
    let rolled_back = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let rollback_flag = rolled_back.clone();
    let (settled_sender, settled_receiver) = tokio::sync::oneshot::channel();
    let settled_sender = Arc::new(Mutex::new(Some(settled_sender)));
    catalog
        .register_named(
            "waiter",
            Plugin::new("waiter", ["loader"], move |context, _| {
                let settlement = context.get(LOADER).expect("loader settlement");
                let sender = settled_sender.clone();
                let observed_rollback = rollback_flag.clone();
                context
                    .own(EffectHandle::synchronous("waiter rollback", {
                        let rollback_flag = rollback_flag.clone();
                        move || {
                            rollback_flag.store(true, std::sync::atomic::Ordering::Release);
                            Ok(())
                        }
                    }))
                    .expect("rollback effect");
                Box::pin(async move {
                    tokio::spawn(async move {
                        let result = settlement.wait().await.map_err(|error| error.to_string());
                        if let Some(sender) = sender.lock().take() {
                            let _ = sender.send((
                                result,
                                observed_rollback.load(std::sync::atomic::Ordering::Acquire),
                            ));
                        }
                    });
                    Ok(())
                })
            }),
        )
        .unwrap();
    catalog
        .register_named(
            "failure",
            Plugin::new("failure", std::iter::empty::<&str>(), |_, _| {
                Box::pin(async { anyhow::bail!("later sibling failed") })
            }),
        )
        .unwrap();
    let context = Context::new();
    let error = catalog
        .load_yaml(
            &context,
            "- id: waiter\n  name: waiter\n- id: failure\n  name: failure\n",
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("later sibling failed"));
    assert!(rolled_back.load(std::sync::atomic::Ordering::Acquire));
    let (settlement, rollback_was_visible) = settled_receiver.await.unwrap();
    assert!(settlement.is_err());
    assert!(rollback_was_visible);
    context.fiber().dispose().await.unwrap();
}
