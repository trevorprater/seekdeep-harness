//! Native startup failures retain their original type and causal chain.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use seekdeep_cordis::{Context, FiberState, Plugin, fiber::EffectHandle};
use seekdeep_schemastery::ValidationError;
use serde_json::{Value, json};

#[tokio::test]
async fn validation_failure_retains_its_cause_and_identity_after_disposal() {
    let context = Context::new();
    let plugin = Plugin::new("validation", std::iter::empty::<String>(), |_, _| {
        Box::pin(async { panic!("invalid configuration reached startup") })
    })
    .with_config_validator(|_| {
        Err(anyhow::Error::new(ValidationError {
            message: "invalid field".to_owned(),
            path: Vec::new(),
        })
        .context("configuration rejected"))
    });
    let mounted = context.plugin(plugin, Value::Null).unwrap();
    let failure = mounted.await_settled().await.unwrap_err();
    assert_eq!(failure.to_string(), "configuration rejected: invalid field");
    assert_eq!(mounted.error(), Some(failure.to_string()));
    let retained = mounted.failure().unwrap();
    assert_eq!(
        retained.downcast_ref::<ValidationError>().unwrap().message,
        "invalid field"
    );
    assert!(Arc::ptr_eq(&retained, &mounted.failure().unwrap()));
    mounted.dispose().await.unwrap();
    assert_eq!(mounted.fiber().state(), FiberState::Disposed);
    assert!(Arc::ptr_eq(&retained, &mounted.failure().unwrap()));
    assert_eq!(
        mounted.await_settled().await.unwrap_err().to_string(),
        "configuration rejected: invalid field"
    );
    context.fiber().restart().await.unwrap();
}

#[tokio::test]
async fn disposal_failure_does_not_replace_the_settled_startup_result() {
    let context = Context::new();
    let plugin = Plugin::new("cleanup", std::iter::empty::<String>(), |ctx, _| {
        Box::pin(async move {
            ctx.own(EffectHandle::synchronous("failed cleanup", || {
                anyhow::bail!("cleanup failure")
            }))?;
            Ok(())
        })
    });
    let mounted = context.plugin(plugin, Value::Null).unwrap();
    mounted.await_settled().await.unwrap();
    assert_eq!(
        mounted.dispose().await.unwrap_err().to_string(),
        "failed cleanup: cleanup failure"
    );
    mounted.await_settled().await.unwrap();
    assert!(mounted.failure().is_none());
    assert!(mounted.error().is_none());
    assert!(context.fiber().restart().await.is_err());
}

#[tokio::test]
async fn startup_failure_is_cleared_by_dependency_loss_and_successful_restart() {
    let context = Context::new();
    let provider = context.provide_named("ready", Arc::new(())).unwrap();
    let plugin = Plugin::new("startup", ["ready"], |_, config| {
        Box::pin(async move {
            if config == json!(true) {
                Ok(())
            } else {
                Err(anyhow::Error::new(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "startup failed",
                ))
                .context("callback rejected"))
            }
        })
    });
    let mounted = context.plugin(plugin, json!(false)).unwrap();
    assert!(mounted.await_settled().await.is_err());
    let first = mounted.failure().unwrap();
    assert_eq!(
        first.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::BrokenPipe
    );
    assert_eq!(
        mounted.error().as_deref(),
        Some("callback rejected: startup failed")
    );
    provider.dispose().await.unwrap();
    mounted.await_settled().await.unwrap();
    assert_eq!(mounted.fiber().state(), FiberState::Pending);
    assert!(mounted.failure().is_none());
    context.provide_named("ready", Arc::new(())).unwrap();
    assert!(mounted.await_settled().await.is_err());
    let next = mounted.failure().unwrap();
    assert!(!Arc::ptr_eq(&first, &next));
    mounted.update(json!(true)).await.unwrap();
    assert!(mounted.failure().is_none());
    assert!(mounted.error().is_none());
    assert_eq!(mounted.fiber().state(), FiberState::Active);
    context.fiber().restart().await.unwrap();
}
