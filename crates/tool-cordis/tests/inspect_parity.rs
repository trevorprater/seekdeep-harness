//! Fiber-state, service ownership, dependency, plugin, and Event inspection parity.

use std::sync::Arc;

use seekdeep_cordis::{Context, Fiber, FiberState, Plugin, ServiceKey};
use seekdeep_tool_cordis::inspect::{
    describe_events, describe_plugins, describe_services, missing_services, provided_services,
    state_label, within_fiber,
};
use serde_json::json;

#[derive(Debug)]
struct Marker;

const PARENT_SERVICE: ServiceKey<Marker> = ServiceKey::new("parentService");
const CHILD_SERVICE: ServiceKey<Marker> = ServiceKey::new("childService");
const DEPENDENCY: ServiceKey<Marker> = ServiceKey::new("dependency");

#[test]
fn state_labels_and_linked_subtree_service_ownership_are_exact() {
    assert_eq!(
        [
            FiberState::Pending,
            FiberState::Loading,
            FiberState::Active,
            FiberState::Failed,
            FiberState::Disposed,
            FiberState::Unloading,
        ]
        .map(state_label),
        [
            "pending",
            "loading",
            "active",
            "failed",
            "disposed",
            "unloading"
        ]
    );
    let context = Context::new();
    let parent = Fiber::child_of("parent", context.fiber());
    let child = Fiber::child_of("child", &parent);
    let parent_context = context.with_fiber(parent.clone());
    let child_context = context.with_fiber(child.clone());
    parent_context
        .provide(PARENT_SERVICE, Arc::new(Marker))
        .unwrap();
    child_context
        .provide(CHILD_SERVICE, Arc::new(Marker))
        .unwrap();
    assert!(within_fiber(&child, &parent));
    assert!(!within_fiber(&parent, &child));
    assert_eq!(
        provided_services(&context, &parent),
        ["childService", "parentService"]
    );
    assert_eq!(provided_services(&context, &child), ["childService"]);
}

#[tokio::test]
async fn missing_dependencies_plugin_lines_services_and_event_contracts_track_live_state() {
    let context = Context::new();
    let plugin = Plugin::new("consumer", ["dependency"], |_, _| {
        Box::pin(async { Ok(()) })
    });
    let fiber = context.plugin(plugin, json!({})).unwrap();
    fiber.await_settled().await.unwrap();
    assert_eq!(missing_services(&context, &fiber), ["dependency"]);
    assert_eq!(describe_plugins(&context), ["- consumer [pending]"]);
    let dependency = context
        .provide(DEPENDENCY, Arc::new(Marker))
        .expect("dependency");
    fiber.await_settled().await.unwrap();
    assert!(missing_services(&context, &fiber).is_empty());
    assert_eq!(describe_plugins(&context), ["- consumer [active]"]);
    let services = describe_services(&context);
    assert!(
        services
            .iter()
            .any(|line| line.starts_with("- dependency "))
    );
    let events = describe_events(Some("agent/pre-step")).unwrap();
    assert!(events[0].starts_with("- agent/pre-step [waterfall]"));
    assert!(events.last().unwrap().contains("MUST call it to delegate"));
    dependency.dispose().await.unwrap();
}
