//! Client redirect, Host call, console mirror, plugin-shape, and style parity.

use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use parking_lot::Mutex;
use seekdeep_cordis_client_runner::*;
use seekdeep_cordis_dynamic_types::CordisDynamicPluginId;
use serde_json::{Value, json};

#[test]
fn browser_redirects_harness_split_and_parse_teaching_are_exact() {
    assert_eq!(
        CLIENT_BUILTIN_INSPECTION
            .iter()
            .map(|entry| entry.name)
            .collect::<Vec<_>>(),
        ["ctx", "React", "host", "styles", "console"]
    );
    assert_eq!(DYNAMIC_CLIENT_REDIRECTS.len(), 6);
    for name in ["setTimeout", "setInterval", "clearTimeout", "clearInterval"] {
        let failure = client_redirect_failure(name).unwrap();
        assert!(failure.contains("browser timer globals are unavailable"));
        assert!(failure.contains("React.useEffect"));
    }
    assert!(
        client_redirect_failure("fetch")
            .unwrap()
            .contains("network belongs to the HOST half")
    );
    assert!(
        client_redirect_failure("require")
            .unwrap()
            .contains("React arrives as the `React` closure symbol")
    );
    assert!(client_redirect_failure("process").is_none());
    assert!(harness_split_failure("handle").contains("harness.handle belongs to the HOST half"));
    let parse = client_parse_failure("unexpected token");
    assert!(parse.contains("client half failed to parse in this browser"));
    assert!(parse.contains("no JSX, no TypeScript"));
}

#[test]
fn plugin_shape_accepts_function_and_apply_object_and_teaches_both_failures() {
    assert_eq!(
        classify_client_plugin(ClientPluginCandidate::Function).unwrap(),
        EvaluatedClientPlugin::Function
    );
    assert_eq!(
        classify_client_plugin(ClientPluginCandidate::Object {
            has_apply: true,
            inject: vec!["slots".to_owned()],
        })
        .unwrap(),
        EvaluatedClientPlugin::Object {
            inject: vec!["slots".to_owned()]
        }
    );
    assert!(
        classify_client_plugin(ClientPluginCandidate::Undefined)
            .unwrap_err()
            .contains("did you forget `return`")
    );
    for candidate in [
        ClientPluginCandidate::Other,
        ClientPluginCandidate::Object {
            has_apply: false,
            inject: Vec::new(),
        },
    ] {
        assert!(
            classify_client_plugin(candidate)
                .unwrap_err()
                .contains("must `return` a plugin")
        );
    }
}

#[tokio::test]
async fn host_call_routes_method_and_defaults_an_omitted_argument_to_null() {
    let calls = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
    let observed = calls.clone();
    let host = ClientHost::new(Arc::new(move |method, args| {
        let observed = observed.clone();
        Box::pin(async move {
            observed.lock().push((method, args));
            Ok(json!({"ok": 1}))
        })
    }));
    assert_eq!(
        host.call("ping", Some(json!({"a": 1}))).await.unwrap(),
        json!({"ok": 1})
    );
    host.call("listServices", None).await.unwrap();
    assert_eq!(
        *calls.lock(),
        [
            ("ping".to_owned(), json!({"a": 1})),
            ("listServices".to_owned(), Value::Null),
        ]
    );
}

#[test]
fn console_mirror_includes_only_classified_values_and_bounds_utf16() {
    assert_eq!(
        mirror_console_error(&[
            ClientConsoleArgument::String("text".to_owned()),
            ClientConsoleArgument::Error("boom".to_owned()),
            ClientConsoleArgument::Json(json!({"a": 1})),
            ClientConsoleArgument::Undefined,
            ClientConsoleArgument::Unserializable,
        ]),
        "text boom {\"a\":1} undefined [unserializable console argument]"
    );
    assert_eq!(
        mirror_console_error(&[ClientConsoleArgument::String("x".repeat(900))])
            .encode_utf16()
            .count(),
        500
    );
}

#[derive(Default)]
struct FakeDom {
    next: AtomicU64,
    live: Mutex<BTreeMap<StyleTagId, (String, String)>>,
}

impl StyleDom for FakeDom {
    fn insert(&self, plugin_id: &CordisDynamicPluginId, css: &str) -> anyhow::Result<StyleTagId> {
        let id = StyleTagId::new(self.next.fetch_add(1, Ordering::AcqRel) + 1);
        self.live
            .lock()
            .insert(id, (plugin_id.to_string(), css.to_owned()));
        Ok(id)
    }

    fn remove(&self, tag: StyleTagId) {
        self.live.lock().remove(&tag);
    }
}

#[test]
fn styles_stamp_ownership_dispose_one_or_all_and_reject_non_strings() {
    let dom = Arc::new(FakeDom::default());
    let styles = DynamicCordisStyles::new(CordisDynamicPluginId::new("dyn-1"), dom.clone());
    let first = styles.insert(".a { color: red }").unwrap();
    styles.insert(".b { color: blue }").unwrap();
    assert_eq!(styles.count(), 2);
    assert_eq!(
        dom.live.lock().values().cloned().collect::<Vec<_>>(),
        [
            ("dyn-1".to_owned(), ".a { color: red }".to_owned()),
            ("dyn-1".to_owned(), ".b { color: blue }".to_owned()),
        ]
    );
    first.dispose();
    first.dispose();
    assert_eq!(styles.count(), 1);
    assert!(styles.insert_value(&json!(42)).is_err());
    styles.dispose();
    assert_eq!(styles.count(), 0);
    assert!(dom.live.lock().is_empty());
}
