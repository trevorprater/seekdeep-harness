//! Host Gateway registration and source-compatible Remote dispatch.

use seekdeep_api_gateway::{GatewayRpcResult, install, register_invocable_service};
use seekdeep_cordis::Context;
use seekdeep_cordis_host_runner::{
    DYNAMIC_CORDIS_RUNNER, DynamicCordisRunner, DynamicCordisRunnerConfig,
};
use seekdeep_llm::AbortSignal;
use seekdeep_typert_registry::TypertRegistry;
use serde_json::json;

#[tokio::test]
async fn dynamic_runner_registers_and_dispatches_inventory_over_the_shared_gateway() {
    let context = Context::new();
    TypertRegistry::new().provide(&context).unwrap();
    let (_services, gateway) = install(&context).unwrap();
    DynamicCordisRunner::try_install(&context, DynamicCordisRunnerConfig::default()).unwrap();
    register_invocable_service(&context, DYNAMIC_CORDIS_RUNNER).unwrap();

    assert!(gateway.claims_endpoint("dynamicCordisRunner/inventory"));
    assert_eq!(
        gateway
            .invoke_rpc(
                "dynamicCordisRunner/inventory",
                json!({ "args": {} }),
                AbortSignal::default(),
            )
            .await,
        GatewayRpcResult::Success {
            value: Some(json!([])),
        }
    );
    assert!(context.get(DYNAMIC_CORDIS_RUNNER).is_some());
}
