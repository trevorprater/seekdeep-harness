//! SDK-facing JSON-RPC server over a booted `SeekDeep` Harness context.

mod server;

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use seekdeep_cordis::{Context, Plugin, fiber::EffectHandle};
use seekdeep_sdk_protocol::{BoxedJsonRpcInput, BoxedJsonRpcOutput, JsonRpcLineTransport};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use server::{HarnessSdkJsonRpcServer, HarnessSdkJsonRpcServerOptions, success_status};

/// Loader plugin name.
pub const NAME: &str = "sdk-jsonrpc-server";
/// Only an Agent factory is required; initialize reads the optional LLM service.
pub const INJECT: &[&str] = &["agents"];

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
    let transport = JsonRpcLineTransport::from_boxed(runtime.input, runtime.output);
    let server = HarnessSdkJsonRpcServer::new(
        context,
        &transport,
        HarnessSdkJsonRpcServerOptions {
            max_tokens_as_success: config.max_tokens_as_success,
        },
    )?;
    let exit_started = Arc::new(AtomicBool::new(false));
    let exit_server = Arc::clone(&server);
    transport.on_request(Arc::new(move |method, params| {
        let server = Arc::clone(&exit_server);
        Box::pin(async move { server.handle_request(&method, params).await })
    }));
    let root = Arc::clone(context.root_fiber());
    let exit = runtime.exit;
    let exit_transport = Arc::clone(&transport);
    transport.on_response_written(Arc::new(move |method, succeeded| {
        if method != "shutdown" || !succeeded || exit_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let root = Arc::clone(&root);
        let exit = Arc::clone(&exit);
        let transport = Arc::clone(&exit_transport);
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            transport.when_incoming_idle().await;
            let _ = transport.flush().await;
            let _ = root.dispose().await;
            exit(0);
        });
    }));
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
        },
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
