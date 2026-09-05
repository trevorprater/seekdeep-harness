//! Runtime topology and discovery contracts ported from `topology.spec.ts`.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use futures::{FutureExt, stream};
use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventArgs, EventOptions, EventReply};
use seekdeep_invariants::InvariantError;
use seekdeep_llm::{
    AdapterStream, LlmAdapter, LlmConfigurableProvider, LlmDiscoveredModel, LlmError,
    LlmModelDiscoveryRequest, LlmProviderAuthentication, LlmRuntime,
};

#[derive(Debug)]
struct EmptyAdapter;

#[async_trait::async_trait]
impl LlmAdapter for EmptyAdapter {
    fn stream(&self, _options: seekdeep_llm::GenerateOptions) -> AdapterStream {
        AdapterStream::new(stream::empty())
    }
}

fn directory_entry(provider: &str) -> LlmConfigurableProvider {
    LlmConfigurableProvider {
        provider: provider.into(),
        display_name: provider.to_owned(),
        settings_ns: "llm-example".to_owned(),
        settings_path: vec!["providers".to_owned(), provider.to_owned()],
        authentication: LlmProviderAuthentication::ProviderNative,
        declared: None,
    }
}

fn llm_code(error: &anyhow::Error) -> Option<&str> {
    error.downcast_ref::<LlmError>().map(LlmError::code)
}

#[tokio::test]
async fn adapter_events_publish_committed_atomic_topology_and_contain_observers() {
    let context = Context::new();
    let runtime = LlmRuntime::install(&context).expect("runtime");
    let observed = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
    let observed_for_listener = observed.clone();
    let runtime_for_listener = runtime.clone();
    context
        .events()
        .on_sync(
            &context,
            "llm/adapters-updated",
            move |_, _| {
                observed_for_listener.lock().push(
                    runtime_for_listener
                        .list_providers()
                        .into_iter()
                        .map(|provider| provider.id.to_string())
                        .collect(),
                );
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .expect("observer");

    let handle = runtime
        .register_adapter(&["a".to_owned(), "b".to_owned()], Arc::new(EmptyAdapter))
        .expect("register");
    assert_eq!(*observed.lock(), [vec!["a".to_owned(), "b".to_owned()]]);
    handle
        .replace(&["b".to_owned(), "c".to_owned()])
        .expect("atomic replace");
    assert_eq!(
        observed.lock().last(),
        Some(&vec!["b".to_owned(), "c".to_owned()])
    );
    handle.dispose().await.expect("dispose");
    assert_eq!(observed.lock().last(), Some(&Vec::new()));

    let later_calls = Arc::new(AtomicUsize::new(0));
    context
        .events()
        .on_sync(
            &context,
            "llm/adapters-updated",
            |_, _| anyhow::bail!("broken observer"),
            EventOptions::default(),
        )
        .expect("throwing observer");
    let later_calls_for_listener = later_calls.clone();
    context
        .events()
        .on_sync(
            &context,
            "llm/adapters-updated",
            move |_, _| {
                later_calls_for_listener.fetch_add(1, Ordering::AcqRel);
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .expect("later observer");
    runtime
        .register_adapter(&["survives".to_owned()], Arc::new(EmptyAdapter))
        .expect("observer cannot veto commit");
    assert_eq!(later_calls.load(Ordering::Acquire), 1);
    assert!(
        runtime
            .list_providers()
            .iter()
            .any(|item| item.id.as_str() == "survives")
    );
}

#[tokio::test]
async fn async_observer_rejection_is_detached_and_invariant_failure_is_rethrown_last() {
    let context = Context::new();
    let runtime = LlmRuntime::install(&context).expect("runtime");
    context
        .events()
        .on(
            &context,
            "llm/adapters-updated",
            |_, _| {
                async {
                    tokio::task::yield_now().await;
                    anyhow::bail!("async observer")
                }
                .boxed()
            },
            EventOptions::default(),
        )
        .expect("async observer");
    runtime
        .register_adapter(&["async-safe".to_owned()], Arc::new(EmptyAdapter))
        .expect("async rejection cannot veto");
    tokio::task::yield_now().await;
    assert!(
        runtime
            .list_providers()
            .iter()
            .any(|item| item.id.as_str() == "async-safe")
    );

    let later_calls = Arc::new(AtomicUsize::new(0));
    context
        .events()
        .on_sync(
            &context,
            "llm/adapters-updated",
            |_, _| {
                Err(InvariantError::new("@deepseek-ai/seekdeep-test", "registry incoherent").into())
            },
            EventOptions::default(),
        )
        .expect("invariant observer");
    let later_calls_for_listener = later_calls.clone();
    context
        .events()
        .on_sync(
            &context,
            "llm/adapters-updated",
            move |_, _| {
                later_calls_for_listener.fetch_add(1, Ordering::AcqRel);
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .expect("later observer");

    let error = runtime
        .register_adapter(
            &["committed-before-error".to_owned()],
            Arc::new(EmptyAdapter),
        )
        .expect_err("invariant is rethrown");
    assert!(error.to_string().contains("registry incoherent"));
    assert_eq!(later_calls.load(Ordering::Acquire), 1);
    assert!(
        runtime
            .list_providers()
            .iter()
            .any(|item| item.id.as_str() == "committed-before-error")
    );
}

#[tokio::test]
async fn configurable_directory_replacement_is_detached_atomic_and_lifecycle_owned() {
    let context = Context::new();
    let runtime = LlmRuntime::install(&context).expect("runtime");
    let source = directory_entry("first");
    let handle = runtime
        .register_configurable_providers(&[source.clone(), directory_entry("second")])
        .expect("directory");
    let elsewhere = runtime
        .register_configurable_providers(&[directory_entry("elsewhere")])
        .expect("other contribution");

    let mut listed = runtime.list_configurable_providers();
    listed[0].display_name = "mutated".to_owned();
    listed[0].settings_path.push("mutated".to_owned());
    assert_eq!(runtime.list_configurable_providers()[0], source);

    let collision = handle
        .replace(&[directory_entry("elsewhere")])
        .expect_err("collision must leave old contribution");
    assert_eq!(llm_code(&collision), Some("DUPLICATE_DIRECTORY"));
    assert_eq!(runtime.list_configurable_providers().len(), 3);

    let mut renamed = directory_entry("first");
    renamed.display_name = "Renamed".to_owned();
    handle
        .replace(&[renamed.clone()])
        .expect("atomic replacement");
    assert_eq!(
        runtime
            .list_configurable_providers()
            .into_iter()
            .find(|item| item.provider.as_str() == "first"),
        Some(renamed)
    );
    handle.replace(&[]).expect("live empty replacement");
    assert_eq!(
        runtime
            .list_configurable_providers()
            .into_iter()
            .map(|item| item.provider.to_string())
            .collect::<Vec<_>>(),
        ["elsewhere"]
    );
    handle.dispose().await.expect("dispose");
    let disposed = handle
        .replace(&[directory_entry("first")])
        .expect_err("disposed handle");
    assert_eq!(llm_code(&disposed), Some("REGISTRATION_DISPOSED"));
    let mut invalid_after_dispose = directory_entry("invalid");
    invalid_after_dispose.settings_ns.clear();
    let disposed_precedes_validation = handle
        .replace(&[invalid_after_dispose])
        .expect_err("disposed check precedes candidate validation");
    assert_eq!(
        llm_code(&disposed_precedes_validation),
        Some("REGISTRATION_DISPOSED")
    );
    elsewhere.dispose().await.expect("dispose other");
}

#[tokio::test]
async fn model_discovery_validates_requests_deduplicates_and_disposes_exact_offer() {
    let context = Context::new();
    let runtime = LlmRuntime::install(&context).expect("runtime");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let requests_for_discovery = requests.clone();
    let discovery = runtime
        .register_model_discovery("llm-example", move |request| {
            requests_for_discovery.lock().push(request);
            async {
                Ok(vec![
                    LlmDiscoveredModel {
                        id: "keep".into(),
                        name: Some("Keep".to_owned()),
                        context_window: Some(1_024),
                        max_tokens: Some(256),
                    },
                    LlmDiscoveredModel {
                        id: "".into(),
                        name: None,
                        context_window: None,
                        max_tokens: None,
                    },
                    LlmDiscoveredModel {
                        id: "keep".into(),
                        name: Some("Duplicate".to_owned()),
                        context_window: None,
                        max_tokens: None,
                    },
                    LlmDiscoveredModel {
                        id: "bare".into(),
                        name: None,
                        context_window: None,
                        max_tokens: None,
                    },
                ])
            }
            .boxed()
        })
        .expect("discovery");

    let invalid = runtime
        .discover_models("llm-example", LlmModelDiscoveryRequest::default())
        .await
        .expect_err("route or endpoint required");
    assert_eq!(llm_code(&invalid), Some("INVALID_DISCOVERY"));
    let discovered = runtime
        .discover_models(
            "llm-example",
            LlmModelDiscoveryRequest {
                provider: Some("known-route".into()),
                ..LlmModelDiscoveryRequest::default()
            },
        )
        .await
        .expect("discover");
    assert_eq!(
        discovered
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["keep", "bare"]
    );
    assert_eq!(requests.lock().len(), 1);

    discovery.dispose().await.expect("dispose");
    let absent = runtime
        .discover_models(
            "llm-example",
            LlmModelDiscoveryRequest {
                provider: Some("known-route".into()),
                ..LlmModelDiscoveryRequest::default()
            },
        )
        .await
        .expect_err("withdrawn offer");
    assert_eq!(llm_code(&absent), Some("NO_DISCOVERY"));
}

#[test]
fn empty_duplicate_and_invalid_registrations_are_all_or_nothing() {
    let context = Context::new();
    let runtime = LlmRuntime::install(&context).expect("runtime");
    let empty = runtime
        .register_adapter(&[], Arc::new(EmptyAdapter))
        .expect_err("empty routes");
    assert_eq!(llm_code(&empty), Some("INVALID_ADAPTER"));
    let duplicate = runtime
        .register_adapter(
            &["same".to_owned(), "same".to_owned()],
            Arc::new(EmptyAdapter),
        )
        .expect_err("duplicate route");
    assert_eq!(llm_code(&duplicate), Some("DUPLICATE_ADAPTER"));
    assert!(runtime.list_providers().is_empty());

    let empty_directory = runtime
        .register_configurable_providers(&[])
        .expect_err("empty directory");
    assert_eq!(llm_code(&empty_directory), Some("INVALID_DIRECTORY"));
    let mut invalid = directory_entry("invalid");
    invalid.settings_path.push(String::new());
    let directory_error = runtime
        .register_configurable_providers(&[directory_entry("valid-first"), invalid])
        .expect_err("all or nothing");
    assert_eq!(llm_code(&directory_error), Some("INVALID_DIRECTORY"));
    assert!(runtime.list_configurable_providers().is_empty());

    assert!(
        runtime
            .register_model_discovery("", |_| async { Ok(Vec::new()) }.boxed())
            .is_err()
    );
    runtime
        .register_model_discovery("unique", |_| async { Ok(Vec::new()) }.boxed())
        .expect("first discovery");
    assert!(
        runtime
            .register_model_discovery("unique", |_| async { Ok(Vec::new()) }.boxed())
            .is_err()
    );

    context
        .events()
        .emit(&context, "unrelated", &EventArgs::new())
        .expect("context remains usable");
}

#[tokio::test]
async fn adapter_registration_can_empty_repopulate_dispose_and_reregister_routes() {
    let context = Context::new();
    let runtime = LlmRuntime::install(&context).expect("runtime");
    let handle = runtime
        .register_adapter(&["m1".to_owned()], Arc::new(EmptyAdapter))
        .expect("initial registration");
    assert_eq!(
        runtime
            .list_providers()
            .into_iter()
            .map(|provider| provider.id.to_string())
            .collect::<Vec<_>>(),
        ["m1"]
    );

    handle.replace(&[]).expect("live empty route set");
    assert!(runtime.list_providers().is_empty());
    handle
        .replace(&["m2".to_owned()])
        .expect("repopulate live registration");
    assert_eq!(runtime.list_providers()[0].id.as_str(), "m2");
    handle.dispose().await.expect("dispose registration");
    assert!(runtime.list_providers().is_empty());

    let disposed = handle
        .replace(&["leaked".to_owned()])
        .expect_err("disposed registration cannot replace routes");
    assert_eq!(llm_code(&disposed), Some("REGISTRATION_DISPOSED"));
    assert!(runtime.list_providers().is_empty());

    let again = runtime
        .register_adapter(&["m2".to_owned()], Arc::new(EmptyAdapter))
        .expect("same route can be registered again");
    assert_eq!(runtime.list_providers()[0].id.as_str(), "m2");
    again.dispose().await.expect("dispose replacement");
    assert!(runtime.list_providers().is_empty());
}
