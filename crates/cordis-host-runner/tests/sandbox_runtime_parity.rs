//! Guarded dynamic Host worker runtime parity.

use std::sync::Arc;

use seekdeep_cordis::{Context, ServiceKey};
use seekdeep_loader::compile_dynamic_host_plugin;
use serde_json::{Value, json};

const PHASE: ServiceKey<Value> = ServiceKey::new("phase");
const RESULT: ServiceKey<Value> = ServiceKey::new("result");

#[tokio::test]
async fn declared_service_property_and_optional_get_activate_through_the_guard() {
    let plugin = compile_dynamic_host_plugin(
        concat!(
            "return {\n",
            "  name: 'dynamic-reader',\n",
            "  inject: ['phase'],\n",
            "  apply(ctx) { ctx.provide('result', { direct: ctx.phase.value, optional: ctx.get('phase').value }); },\n",
            "};\n",
        ),
        5_000,
    )
    .unwrap();
    let context = Context::new();
    context
        .provide(PHASE, Arc::new(json!({"value": 42})))
        .unwrap();
    let fiber = context.plugin(plugin, Value::Null).unwrap();
    fiber.await_settled().await.unwrap();
    assert_eq!(
        context.get(RESULT).as_deref(),
        Some(&json!({"direct": 42, "optional": 42}))
    );
    fiber.dispose().await.unwrap();
    assert!(context.get(RESULT).is_none());
}

#[tokio::test]
async fn undeclared_property_and_facade_assignment_fail_without_registration() {
    let context = Context::new();
    context
        .provide(PHASE, Arc::new(json!({"value": 42})))
        .unwrap();
    for body in [
        "return { apply(ctx) { ctx.provide('result', ctx.phase.value); } };",
        "return { apply(ctx) { ctx.anything = 1; } };",
    ] {
        let plugin = compile_dynamic_host_plugin(body, 5_000).unwrap();
        let fiber = context.plugin(plugin, Value::Null).unwrap();
        let error = fiber.await_settled().await.unwrap_err();
        assert!(
            error.to_string().contains("is not injected")
                || error.to_string().contains("read-only")
        );
        assert!(context.get(RESULT).is_none());
        fiber.dispose().await.unwrap();
    }
}

#[tokio::test]
async fn framework_internals_are_withheld_and_reachability_matches_the_facade() {
    let context = Context::new();
    for member in [
        "root",
        "parent",
        "scope",
        "fiber",
        "reflect",
        "registry",
        "events",
        "extend",
        "isolate",
        "intercept",
        "plugin",
        "set",
        "mixin",
    ] {
        let plugin = compile_dynamic_host_plugin(
            &format!("return {{ apply(ctx) {{ const value = ctx.{member}; }} }};"),
            5_000,
        )
        .unwrap();
        let fiber = context.plugin(plugin, Value::Null).unwrap();
        let error = fiber.await_settled().await.unwrap_err().to_string();
        assert!(error.contains(&format!("sandbox ctx does not expose \"{member}\"")));
        assert!(error.contains("withheld by design"));
        fiber.dispose().await.unwrap();
    }

    let plugin = compile_dynamic_host_plugin(
        concat!(
            "return { apply(ctx) { ctx.provide('result', {",
            "symbol: ctx[Symbol.iterator] === undefined,",
            "tools: 'tools' in ctx, on: 'on' in ctx, root: 'root' in ctx,",
            "missing: ctx.get('missing') === undefined,",
            "}); } };",
        ),
        5_000,
    )
    .unwrap();
    let fiber = context.plugin(plugin, Value::Null).unwrap();
    fiber.await_settled().await.unwrap();
    assert_eq!(
        context.get(RESULT).as_deref(),
        Some(&json!({
            "symbol": true,
            "tools": true,
            "on": true,
            "root": false,
            "missing": true,
        }))
    );
    fiber.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn sandbox_has_no_node_globals_and_supplies_base64_and_console() {
    let plugin = compile_dynamic_host_plugin(
        concat!(
            "return { apply(ctx) {\n",
            "  console.info('sandbox');\n",
            "  ctx.provide('result', {\n",
            "    process: typeof process, buffer: typeof Buffer,\n",
            "    encoded: btoa('abc'), decoded: atob('YWJj'),\n",
            "  });\n",
            "} };\n",
        ),
        5_000,
    )
    .unwrap();
    let context = Context::new();
    let fiber = context.plugin(plugin, Value::Null).unwrap();
    fiber.await_settled().await.unwrap();
    assert_eq!(
        context.get(RESULT).as_deref(),
        Some(&json!({
            "process": "undefined",
            "buffer": "undefined",
            "encoded": "YWJj",
            "decoded": "abc",
        }))
    );
    fiber.dispose().await.unwrap();
}

#[test]
fn synchronous_infinite_body_is_bounded_during_load() {
    let error = compile_dynamic_host_plugin("while (true) {}", 1).unwrap_err();
    assert!(error.to_string().contains("timed out"));
}
