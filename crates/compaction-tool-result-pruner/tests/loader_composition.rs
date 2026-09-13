//! Real declarative Loader composition for the tool-result pruner.

use seekdeep_cordis::Context;
use seekdeep_loader::PluginCatalog;

fn catalog() -> PluginCatalog {
    let catalog = PluginCatalog::new();
    catalog
        .register_named("seekdeep-token-meter", seekdeep_token_meter::plugin())
        .expect("register token meter");
    catalog
        .register_named(
            "seekdeep-compaction-tool-result-pruner",
            seekdeep_compaction_tool_result_pruner::plugin(),
        )
        .expect("register pruner");
    catalog
}

#[tokio::test]
async fn yaml_loads_flat_shape_and_rejects_stale_config() -> anyhow::Result<()> {
    let context = Context::new();
    let composition = catalog()
        .load_yaml(
            &context,
            concat!(
                "- id: meter\n",
                "  name: seekdeep-token-meter\n",
                "- id: pruner\n",
                "  name: seekdeep-compaction-tool-result-pruner\n",
                "  config:\n",
                "    thresholdChars: 100\n",
                "    headChars: 20\n",
                "    tailChars: 10\n",
            ),
        )
        .await?;
    let pruner = context
        .get(seekdeep_compaction_tool_result_pruner::TOOL_RESULT_PRUNER)
        .ok_or_else(|| anyhow::anyhow!("pruner missing"))?;
    assert_eq!(pruner.config.threshold_chars, 100);
    assert_eq!(pruner.config.head_chars, 20);
    assert_eq!(pruner.config.tail_chars, 10);
    composition.dispose().await?;
    context.fiber().dispose().await?;

    let stale_context = Context::new();
    let error = catalog()
        .load_yaml(
            &stale_context,
            concat!(
                "- id: meter\n",
                "  name: seekdeep-token-meter\n",
                "- id: pruner\n",
                "  name: seekdeep-compaction-tool-result-pruner\n",
                "  config:\n",
                "    maxChars: 100\n",
            ),
        )
        .await
        .expect_err("stale config");
    assert!(error.to_string().contains("maxChars"), "{error:#}");
    assert!(
        stale_context
            .get(seekdeep_compaction_tool_result_pruner::TOOL_RESULT_PRUNER)
            .is_none()
    );
    stale_context.fiber().dispose().await
}
