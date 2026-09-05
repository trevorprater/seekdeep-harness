//! Real Loader composition and load-time schema rejection parity.

use seekdeep_compaction::service::COMPACTION;
use seekdeep_compaction_tool_result_pruner::TOOL_RESULT_PRUNER;
use seekdeep_cordis::{Context, FiberState, Plugin};
use seekdeep_llm::LlmRuntime;
use seekdeep_loader::PluginCatalog;
use serde_json::{Value, json};

fn llm_plugin() -> Plugin {
    Plugin::new("llm", std::iter::empty::<&str>(), |context, _| {
        Box::pin(async move {
            LlmRuntime::install(&context)?;
            Ok(())
        })
    })
}

fn catalog() -> PluginCatalog {
    let catalog = PluginCatalog::new();
    for (name, plugin) in [
        ("@seekdeep-ai/seekdeep-llm", llm_plugin()),
        (
            "@seekdeep-ai/seekdeep-session",
            seekdeep_core::session_store::plugin(),
        ),
        (
            "@seekdeep-ai/seekdeep-token-meter",
            seekdeep_token_meter::plugin(),
        ),
        (
            "@seekdeep-ai/seekdeep-compaction-tool-result-pruner",
            seekdeep_compaction_tool_result_pruner::plugin(),
        ),
        (
            "@seekdeep-ai/seekdeep-compaction-basic",
            seekdeep_compaction_basic::index::plugin(),
        ),
    ] {
        catalog.register_named(name, plugin).unwrap();
    }
    catalog
}

async fn compaction_dependencies(context: &Context) {
    let fibers = [
        context.plugin(llm_plugin(), Value::Null).unwrap(),
        context
            .plugin(seekdeep_core::session_store::plugin(), Value::Null)
            .unwrap(),
        context
            .plugin(seekdeep_token_meter::plugin(), json!({}))
            .unwrap(),
    ];
    for fiber in fibers {
        fiber.await_settled().await.unwrap();
    }
}

async fn plugin_error(context: &Context, plugin: Plugin, config: Value) -> String {
    let fiber = context.plugin(plugin, config).unwrap();
    let error = fiber.await_settled().await.unwrap_err().to_string();
    fiber.dispose().await.unwrap();
    error
}

#[tokio::test]
async fn loads_shipped_token_meter_pruner_and_compaction_order() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("cordis.yml");
    std::fs::write(
        &path,
        concat!(
            "- name: '@seekdeep-ai/seekdeep-llm'\n",
            "- name: '@seekdeep-ai/seekdeep-session'\n",
            "- name: '@seekdeep-ai/seekdeep-token-meter'\n",
            "- name: '@seekdeep-ai/seekdeep-compaction-tool-result-pruner'\n",
            "  config:\n",
            "    thresholdChars: 100\n",
            "    headChars: 20\n",
            "    tailChars: 10\n",
            "- name: '@seekdeep-ai/seekdeep-compaction-basic'\n",
            "  config:\n",
            "    thresholdRatio: 0.5\n",
            "    retainRatio: 0.125\n",
            "    auto: false\n",
        ),
    )
    .unwrap();
    let context = Context::new();
    let composition = catalog().load_file(&context, &path).await.unwrap();

    assert!(
        composition.entries().iter().all(|entry| {
            entry.disabled || entry.group || entry.state == Some(FiberState::Active)
        })
    );
    assert!(context.get(TOOL_RESULT_PRUNER).is_some());
    assert!(context.get(COMPACTION).is_some());
    let configured = composition
        .fibers()
        .into_iter()
        .find(|fiber| fiber.plugin_name() == "compaction-basic")
        .expect("compaction-basic fiber");
    assert_eq!(
        configured.config(),
        json!({"thresholdRatio": 0.5, "retainRatio": 0.125, "auto": false})
    );

    composition.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn rejects_stale_token_meter_config_after_schema_normalization() {
    let context = Context::new();
    let error = plugin_error(
        &context,
        seekdeep_token_meter::plugin(),
        json!({"contextWindow": 4096}),
    )
    .await;
    assert!(error.contains("TokenMeterConfig: unknown key \"contextWindow\""));
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn rejects_stale_compaction_config_after_schema_normalization() {
    let context = Context::new();
    compaction_dependencies(&context).await;
    let error = plugin_error(
        &context,
        seekdeep_compaction_basic::index::plugin(),
        json!({"models": {"legacy": {"thresholdRatio": 0.5}}}),
    )
    .await;
    assert!(
        error.contains("BasicCompactionConfig: unknown key \"models\""),
        "{error}"
    );
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn rejects_capacity_independent_merged_ratio_conflict_during_load() {
    let context = Context::new();
    compaction_dependencies(&context).await;
    let error = plugin_error(
        &context,
        seekdeep_compaction_basic::index::plugin(),
        json!({
            "retainRatio": 0.2,
            "modelPolicies": [{
                "provider": "test-provider",
                "model": "test-model",
                "thresholdRatio": 0.1
            }]
        }),
    )
    .await;
    assert!(error.contains("modelPolicies[0]"), "{error}");
    assert!(error.contains("retainRatio (0.2)"), "{error}");
    assert!(error.contains("thresholdRatio (0.1)"), "{error}");
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn rejects_incomplete_model_policy_summarization_pair_during_load() {
    let context = Context::new();
    compaction_dependencies(&context).await;
    let error = plugin_error(
        &context,
        seekdeep_compaction_basic::index::plugin(),
        json!({
            "summarizationProvider": "default-provider",
            "summarizationModel": "default-model",
            "modelPolicies": [{
                "provider": "test-provider",
                "model": "test-model",
                "summarizationModel": ""
            }]
        }),
    )
    .await;
    assert!(error.contains("modelPolicies[0]"), "{error}");
    assert!(error.contains("must be set together"), "{error}");
    context.fiber().dispose().await.unwrap();
}
