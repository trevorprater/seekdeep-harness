//! Actual file-plugin Tool callbacks preserve arbitrary JSON and UTF-16 strings.

use seekdeep_code_runtime::CodeJsonValue;
use seekdeep_cordis::Context;
use seekdeep_llm::{AbortSignal, CallId, ContentBlock};
use seekdeep_loader::PluginCatalog;
use seekdeep_tools::{
    TOOLS, ToolExecutionInput, ToolExecutionMode, ToolRuntime, ToolRuntimeConfig,
};

#[tokio::test]
async fn node_tools_keep_raw_arguments_results_rendering_metadata_and_errors() -> anyhow::Result<()>
{
    let temporary = tempfile::tempdir()?;
    let module = temporary.path().join("tools.mjs");
    std::fs::write(
        &module,
        r"
export const inject = ['tools'];
export function apply(ctx) {
  ctx.tools.register({
    name: 'node_json', description: 'Echo arbitrary JSON through the real Node module realm.',
    parameters: { value: { type: 'object', additionalProperties: true, required: true } },
    output: {
      schema: { type: 'object', additionalProperties: true },
      render(_args, value) { return [{ type: 'text', text: value['\ud800'].text }]; },
      presentationMeta(_args, value) { return value; },
    },
    isConcurrencySafe(args) { return Object.keys(args.value)[0].charCodeAt(0) === 0xd800; },
    async execute(args) { await Promise.resolve(); return args.value; },
  });
  ctx.tools.register({
    name: 'node_error', description: 'Reject with the original UTF-16 message.',
    parameters: {},
    output: { schema: { type: 'object' }, render() { return []; } },
    execute() { throw new Error('\ud800 rejected \udc00'); },
  });
}
",
    )?;
    let context = Context::new();
    let tools = ToolRuntime::new(context.clone(), ToolRuntimeConfig::default())?;
    context.provide(TOOLS, tools.clone())?;
    let composition = PluginCatalog::new()
        .load_yaml_at(
            &context,
            "- id: raw-tools\n  name: ./tools.mjs\n",
            temporary.path().join("cordis.yml"),
        )
        .await?;
    let args = CodeJsonValue::parse(r#"{"value":{"\ud800":{"text":"\udc00"},"pair":"\ud83d\ude00","values":["\ud800",null,false,123]}}"#.to_owned())?;
    let input = ToolExecutionInput::new(
        CallId::new("node-json"),
        "node_json",
        args.clone(),
        AbortSignal::default(),
    );
    assert_eq!(tools.execution_mode(&input), ToolExecutionMode::Parallel);
    let result = tools.execute(input).await;
    assert!(!result.is_error(), "{result:?}");
    let expected = args.get("value").unwrap().to_owned();
    assert_eq!(result.json_value(), Some(&expected));
    assert_eq!(result.meta(), Some(&expected));
    let [ContentBlock::Text { text }] = result.content() else {
        anyhow::bail!("missing raw text content");
    };
    assert_eq!(text.utf16_units(), &[0xdc00]);
    let failed = tools
        .execute(ToolExecutionInput::new(
            CallId::new("node-error"),
            "node_error",
            CodeJsonValue::parse("{}".to_owned())?,
            AbortSignal::default(),
        ))
        .await;
    assert!(failed.is_error());
    assert_eq!(
        failed.error().unwrap().message.utf16_units(),
        &[
            0xd800, 0x20, 0x72, 0x65, 0x6a, 0x65, 0x63, 0x74, 0x65, 0x64, 0x20, 0xdc00
        ]
    );
    composition.dispose().await?;
    assert!(tools.get("node_json", None).is_none());
    assert!(tools.get("node_error", None).is_none());
    context.fiber().dispose().await?;
    Ok(())
}
