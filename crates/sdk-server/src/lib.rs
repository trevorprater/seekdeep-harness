//! SDK-facing JSON-RPC server over a booted `SeekDeep` Harness context.

mod server;

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use seekdeep_cordis::{Context, Plugin, ServiceKey, fiber::EffectHandle};
use seekdeep_sdk_protocol::{BoxedJsonRpcInput, BoxedJsonRpcOutput, JsonRpcLineTransport};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use server::{HarnessSdkJsonRpcServer, HarnessSdkJsonRpcServerOptions, success_status};

/// Loader plugin name.
pub const NAME: &str = "sdk-jsonrpc-server";
/// Only an Agent factory is required; initialize reads the optional LLM service.
pub const INJECT: &[&str] = &["agents"];
/// Exact live SDK server marker used by process runners to avoid competing for stdin.
pub const SDK_JSONRPC_SERVER: ServiceKey<HarnessSdkJsonRpcServer> =
    ServiceKey::new("sdkJsonrpcServer");

/// Deployment-owned status mapping.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Report max-token termination as a successful SDK result.
    pub max_tokens_as_success: bool,
}

/// Runtime-only stream and process-exit hooks.
pub struct ServerRuntime {
    /// Transport input.
    pub input: BoxedJsonRpcInput,
    /// Transport output.
    pub output: BoxedJsonRpcOutput,
    /// Process exit boundary.
    pub exit: Arc<dyn Fn(i32) + Send + Sync>,
    /// Whether transport input closure owns root disposal and process exit.
    pub exit_on_input_failure: bool,
}

/// Serves SDK requests and lifecycle notifications on explicit streams.
///
/// # Errors
///
/// Returns missing-agent, listener, effect-ownership, or server-construction failures.
pub fn apply_with_runtime(
    context: &Context,
    config: Config,
    runtime: ServerRuntime,
) -> anyhow::Result<Arc<HarnessSdkJsonRpcServer>> {
    apply_with_runtime_readiness(context, config, runtime, true)
}

fn apply_with_runtime_readiness(
    context: &Context,
    config: Config,
    runtime: ServerRuntime,
    ready: bool,
) -> anyhow::Result<Arc<HarnessSdkJsonRpcServer>> {
    let transport = JsonRpcLineTransport::from_boxed(runtime.input, runtime.output);
    let options = HarnessSdkJsonRpcServerOptions {
        max_tokens_as_success: config.max_tokens_as_success,
    };
    let server = if ready {
        HarnessSdkJsonRpcServer::new(context, &transport, options)?
    } else {
        HarnessSdkJsonRpcServer::new_deferred(context, &transport, options)?
    };
    let marker = context.provide(SDK_JSONRPC_SERVER, server.clone())?;
    let exit_started = Arc::new(AtomicBool::new(false));
    let exit_server = Arc::clone(&server);
    transport.on_request(Arc::new(move |method, params| {
        let server = Arc::clone(&exit_server);
        Box::pin(async move { server.handle_request(&method, params).await })
    }));
    let root = Arc::clone(context.root_fiber());
    let exit = runtime.exit;
    let response_exit = Arc::clone(&exit);
    let response_started = Arc::clone(&exit_started);
    let exit_transport = Arc::clone(&transport);
    transport.on_response_written(Arc::new(move |method, succeeded| {
        if method != "shutdown" || !succeeded || response_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let root = Arc::clone(&root);
        let exit = Arc::clone(&response_exit);
        let transport = Arc::clone(&exit_transport);
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            transport.when_incoming_idle().await;
            let _ = transport.flush().await;
            let _ = root.dispose().await;
            exit(0);
        });
    }));
    if runtime.exit_on_input_failure {
        let root = Arc::clone(context.root_fiber());
        let exit = Arc::clone(&exit);
        let exit_started = Arc::clone(&exit_started);
        transport.on_input_failure(Arc::new(move |_| {
            if exit_started.swap(true, Ordering::AcqRel) {
                return;
            }
            let root = Arc::clone(&root);
            let exit = Arc::clone(&exit);
            tokio::spawn(async move {
                let _ = root.dispose().await;
                exit(0);
            });
        }));
    }
    transport.start();
    let cleanup_server = Arc::clone(&server);
    let cleanup_transport = Arc::clone(&transport);
    let effect = EffectHandle::new("jsonrpc.serve", move || {
        Box::pin(async move {
            let result = cleanup_server.shutdown().await;
            cleanup_transport.close();
            result.map(|_| ())
        })
    });
    if let Err(error) = context.own(effect) {
        transport.close();
        let _ = futures::executor::block_on(marker.dispose());
        return Err(error.into());
    }
    Ok(server)
}

/// Serves SDK requests over process stdio and exits the process after protocol shutdown.
///
/// # Errors
///
/// Returns ordinary [`apply_with_runtime`] failures.
pub fn apply(context: &Context, config: Config) -> anyhow::Result<Arc<HarnessSdkJsonRpcServer>> {
    apply_with_runtime(
        context,
        config,
        ServerRuntime {
            input: Box::pin(tokio::io::stdin()),
            output: Box::pin(tokio::io::stdout()),
            exit: Arc::new(|code| {
                std::process::exit(code);
            }),
            exit_on_input_failure: true,
        },
    )
}

fn apply_deferred(
    context: &Context,
    config: Config,
) -> anyhow::Result<Arc<HarnessSdkJsonRpcServer>> {
    apply_with_runtime_readiness(
        context,
        config,
        ServerRuntime {
            input: Box::pin(tokio::io::stdin()),
            output: Box::pin(tokio::io::stdout()),
            exit: Arc::new(|code| {
                std::process::exit(code);
            }),
            exit_on_input_failure: true,
        },
        false,
    )
}

fn normalize_config(value: &Value) -> anyhow::Result<Value> {
    if value.is_null() {
        return Ok(serde_json::to_value(Config::default())?);
    }
    Ok(serde_json::to_value(serde_json::from_value::<Config>(
        value.clone(),
    )?)?)
}

/// Builds the Loader-compatible namespace plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, config| {
        Box::pin(async move {
            let config: Config = serde_json::from_value(config)?;
            apply(&context, config)?;
            Ok(())
        })
    })
    .with_config_validator(normalize_config)
}

/// Builds the process-launcher plugin whose wire stays gated until boot commits.
#[must_use]
pub fn deferred_plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, config| {
        Box::pin(async move {
            let config: Config = serde_json::from_value(config)?;
            apply_deferred(&context, config)?;
            Ok(())
        })
    })
    .with_config_validator(normalize_config)
}
