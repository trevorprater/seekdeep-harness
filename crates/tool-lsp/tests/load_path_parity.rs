//! Loader metadata and namespace-style public surface guard.

use seekdeep_cordis::Context;
use seekdeep_tool_lsp::{Config, INJECT, NAME, apply, config_schema, plugin};

#[test]
fn public_namespace_keeps_name_inject_config_apply_and_plugin_metadata() {
    let definition = plugin();
    assert_eq!(NAME, "tool-lsp");
    assert_eq!(INJECT, ["tools", "lsp", "systemPrompt"]);
    assert_eq!(definition.name(), NAME);
    assert_eq!(
        definition.inject(),
        INJECT.iter().map(ToString::to_string).collect::<Vec<_>>()
    );
    let _config = Config::default();
    let _schema = config_schema();
    assert!(apply(&Context::new(), &Config::default()).is_err());
}
