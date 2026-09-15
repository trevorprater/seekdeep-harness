//! Base bundle manifest, patch inventory, and platform-gating parity.

use seekdeep_loader::profile_patch::{JavaScriptExpression, ProfileNode, parse_patch_list_yaml};

const MANIFEST: &str = include_str!("../../../packages/bundle/base/package.json");
const PATCH: &str = include_str!("../../../packages/bundle/base/cordis.patch.yml");

fn rows() -> Vec<indexmap::IndexMap<String, ProfileNode>> {
    let patches = parse_patch_list_yaml(PATCH).unwrap();
    patches
        .iter()
        .flat_map(|patch| {
            patch
                .insert()
                .and_then(ProfileNode::as_sequence)
                .into_iter()
                .flatten()
                .filter_map(ProfileNode::as_mapping)
                .cloned()
        })
        .collect()
}

fn row<'a>(
    rows: &'a [indexmap::IndexMap<String, ProfileNode>],
    id: &str,
) -> &'a indexmap::IndexMap<String, ProfileNode> {
    rows.iter()
        .find(|row| row.get("id").and_then(ProfileNode::as_str) == Some(id))
        .unwrap_or_else(|| panic!("missing base row {id}"))
}

#[test]
fn manifest_declares_the_real_parseable_base_patch_and_expected_inventory() {
    let manifest: serde_json::Value = serde_json::from_str(MANIFEST).unwrap();
    assert_eq!(
        manifest
            .pointer("/seekdeep/bundle/patch")
            .and_then(|value| value.as_str()),
        Some("./cordis.patch.yml")
    );
    let rows = rows();
    assert!(rows.len() > 50);
    assert_eq!(
        row(&rows, "agent-loop")
            .get("id")
            .and_then(ProfileNode::as_str),
        Some("agent-loop")
    );
    let telemetry = row(&rows, "session-telemetry-otel");
    let mode = telemetry
        .get("config")
        .and_then(ProfileNode::as_mapping)
        .and_then(|config| config.get("mode"))
        .and_then(ProfileNode::as_javascript)
        .map(JavaScriptExpression::as_str);
    assert_eq!(
        mode,
        Some("process.env.SEEKDEEP_TELEMETRY_MODE || 'DISABLED'")
    );
    assert!(rows.iter().all(|row| {
        !matches!(
            row.get("id").and_then(ProfileNode::as_str),
            Some("subagent-codex" | "subagent-claude-code")
        )
    }));
    let dependencies = manifest
        .get("dependencies")
        .and_then(serde_json::Value::as_object)
        .unwrap();
    assert!(!dependencies.contains_key("@seekdeep-ai/seekdeep-subagent-codex"));
    assert!(!dependencies.contains_key("@seekdeep-ai/seekdeep-subagent-claude-code"));
}

#[test]
fn shell_stacks_use_symmetric_platform_disabled_expressions() {
    let rows = rows();
    for (id, expression) in [
        ("bash-sandbox", "process.platform === 'win32'"),
        ("tool-bash", "process.platform === 'win32'"),
        ("pwsh-sandbox", "process.platform !== 'win32'"),
        ("tool-pwsh", "process.platform !== 'win32'"),
    ] {
        assert_eq!(
            row(&rows, id)
                .get("disabled")
                .and_then(ProfileNode::as_javascript)
                .map(JavaScriptExpression::as_str),
            Some(expression)
        );
    }
    assert!(
        !std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../packages/bundle/base/windows.cordis.patch.yml")
            .exists()
    );
}
