//! Provider registration order: `list()` reports the oracle's Map iteration order.

use super::support;

use seekdeep_cordis::Context;
use seekdeep_subagent::index::SubagentRuntime;
use support::providers::ScriptedProvider;

#[tokio::test]
async fn lists_providers_in_registration_order() {
    let context = Context::new();
    let registry = SubagentRuntime::new(&context);

    let _alpha = registry
        .register_provider(ScriptedProvider::one_shot_only("alpha"))
        .expect("alpha");
    let beta = registry
        .register_provider(ScriptedProvider::one_shot_only("beta"))
        .expect("beta");
    let _gamma = registry
        .register_provider(ScriptedProvider::one_shot_only("gamma"))
        .expect("gamma");
    assert_eq!(registry.list(), vec!["alpha", "beta", "gamma"]);

    // Disposing the registration effect unregisters: the removal path is where an
    // insertion-ordered map is easiest to get wrong, because IndexMap::remove swaps the last
    // entry into the vacated slot where Map.delete keeps the rest in order.
    beta.dispose().await.expect("unregister beta");

    let _delta = registry
        .register_provider(ScriptedProvider::one_shot_only("delta"))
        .expect("delta");
    assert_eq!(
        registry.list(),
        vec!["alpha", "gamma", "delta"],
        "removal must leave the remaining providers in registration order"
    );
}
