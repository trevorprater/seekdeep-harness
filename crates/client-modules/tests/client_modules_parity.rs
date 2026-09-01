//! Boot manifest and target-neutral lazy module-table parity.

#![cfg(not(target_arch = "wasm32"))]

use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use futures::future::BoxFuture;
use parking_lot::Mutex;
use seekdeep_client_modules::*;
use serde_json::{Value, json};

type Exports = Arc<Value>;

fn row(id: &str) -> BootModuleRow {
    BootModuleRow {
        id: ClientModuleId::new(id),
        url: format!("/plugins/{id}/client.js?rev=0"),
        rev: "0".to_owned(),
    }
}

#[derive(Default)]
struct ScriptedLoader {
    bundles: Mutex<BTreeMap<ClientModuleId, Option<ClientModuleFactory<Exports>>>>,
    fetched: Mutex<Vec<String>>,
    gates: Mutex<BTreeMap<ClientModuleId, Arc<tokio::sync::Notify>>>,
}

impl ScriptedLoader {
    fn bundle(&self, id: &str, factory: ClientModuleFactory<Exports>) {
        self.bundles
            .lock()
            .insert(ClientModuleId::new(id), Some(factory));
    }

    fn missing(&self, id: &str) {
        self.bundles.lock().insert(ClientModuleId::new(id), None);
    }

    fn gate(&self, id: &str) -> Arc<tokio::sync::Notify> {
        let gate = Arc::new(tokio::sync::Notify::new());
        self.gates
            .lock()
            .insert(ClientModuleId::new(id), gate.clone());
        gate
    }
}

impl ClientBundleLoader<Exports> for ScriptedLoader {
    fn load(
        &self,
        row: BootModuleRow,
        registrar: ClientFactoryRegistrar<Exports>,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        self.fetched.lock().push(row.url);
        let bundle = self.bundles.lock().get(&row.id).cloned().flatten();
        let gate = self.gates.lock().get(&row.id).cloned();
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.notified().await;
            }
            if let Some(factory) = bundle {
                registrar.register(row.id, factory)?;
            }
            Ok(())
        })
    }
}

#[derive(Default)]
struct ScriptedStyles {
    owned: Mutex<BTreeMap<ClientModuleId, Vec<String>>>,
}

impl ClientStyleClaimer for ScriptedStyles {
    fn claim(&self, id: &ClientModuleId) -> Vec<String> {
        self.owned.lock().get(id).cloned().unwrap_or_default()
    }
}

fn system(
    rows: Vec<BootModuleRow>,
    seed: Vec<(String, Exports)>,
    loader: Arc<ScriptedLoader>,
    styles: Arc<ScriptedStyles>,
) -> ClientModuleSystem<Exports> {
    ClientModuleSystem::new(rows, seed, loader, styles).unwrap()
}

fn exports(value: Value) -> Exports {
    Arc::new(value)
}

#[test]
fn boot_manifest_projects_both_views_and_rejects_each_malformed_boundary() {
    let manifest = parse_boot_manifest(&json!({
        "rev": "graph-1",
        "entries": [
            {"id": "a", "url": "/a.js", "rev": "a1"},
            {"id": "b", "url": "/b.js", "rev": "b1", "inject": ["a"], "immediately": true}
        ]
    }))
    .unwrap();
    assert_eq!(manifest.rev, "graph-1");
    assert_eq!(manifest.modules[1].id.as_str(), "b");
    assert_eq!(manifest.plugins[0].inject, Vec::<String>::new());
    assert!(!manifest.plugins[0].immediately);
    assert_eq!(manifest.plugins[1].inject, ["a"]);
    assert!(manifest.plugins[1].immediately);

    for (wire, expected) in [
        (Value::Null, "missing or not an object"),
        (json!({"entries": []}), "rev must be a string"),
        (
            json!({"rev": "x", "entries": {}}),
            "entries must be an array",
        ),
        (
            json!({"rev": "x", "entries": [null]}),
            "entry is not an object",
        ),
        (
            json!({"rev": "x", "entries": [{"id": "a", "url": 1, "rev": "r"}]}),
            "must carry string id/url/rev",
        ),
        (
            json!({"rev": "x", "entries": [{"id": "a", "url": "u", "rev": "r", "inject": [1]}]}),
            "inject must be a string array",
        ),
        (
            json!({"rev": "x", "entries": [{"id": "a", "url": "u", "rev": "r", "immediately": 1}]}),
            "immediately must be a boolean",
        ),
    ] {
        let error = parse_boot_manifest(&wire).unwrap_err().to_string();
        assert!(error.contains(expected), "{error}");
    }
}

#[tokio::test]
async fn prefetch_is_lazy_import_memoizes_and_concurrent_arrival_is_shared() {
    let loader = Arc::new(ScriptedLoader::default());
    let styles = Arc::new(ScriptedStyles::default());
    let runs = Arc::new(AtomicUsize::new(0));
    loader.bundle("a", {
        let runs = runs.clone();
        Arc::new(move |_| {
            runs.fetch_add(1, Ordering::AcqRel);
            Ok(exports(json!({"marker": "a"})))
        })
    });
    let modules = system(vec![row("a")], Vec::new(), loader.clone(), styles);
    modules.prefetch(&ClientModuleId::new("a")).await.unwrap();
    assert_eq!(runs.load(Ordering::Acquire), 0);
    assert!(modules.cache_snapshot().is_empty());
    let first = modules.import("a").await.unwrap();
    let second = modules.import("a").await.unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(runs.load(Ordering::Acquire), 1);
    modules.prefetch(&ClientModuleId::new("a")).await.unwrap();
    assert_eq!(loader.fetched.lock().len(), 1);

    let loader = Arc::new(ScriptedLoader::default());
    let gate = loader.gate("gated");
    let runs = Arc::new(AtomicUsize::new(0));
    loader.bundle("gated", {
        let runs = runs.clone();
        Arc::new(move |_| {
            runs.fetch_add(1, Ordering::AcqRel);
            Ok(exports(json!({"marker": "gated"})))
        })
    });
    let modules = system(
        vec![row("gated")],
        Vec::new(),
        loader.clone(),
        Arc::new(ScriptedStyles::default()),
    );
    let first = {
        let modules = modules.clone();
        tokio::spawn(async move { modules.import("gated").await })
    };
    let second = {
        let modules = modules.clone();
        tokio::spawn(async move { modules.import("gated").await })
    };
    while loader.fetched.lock().is_empty() {
        tokio::task::yield_now().await;
    }
    gate.notify_one();
    let first = first.await.unwrap().unwrap();
    let second = second.await.unwrap().unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(loader.fetched.lock().len(), 1);
    assert_eq!(runs.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn require_resolves_seed_static_cache_recursive_factories_and_cycles() {
    let loader = Arc::new(ScriptedLoader::default());
    let seed = exports(json!({"marker": "react"}));
    let shell = exports(json!({"marker": "shell"}));
    loader.bundle("b", Arc::new(|_| Ok(exports(json!({"helper": "from-b"})))));
    loader.bundle(
        "a",
        Arc::new(|require| {
            let b = require.require("b/client")?;
            let react = require.require("react")?;
            let shell = require.require("app-shell")?;
            Ok(exports(json!({
                "b": b["helper"],
                "react": react["marker"],
                "shell": shell["marker"],
            })))
        }),
    );
    let modules = system(
        vec![row("a"), row("b")],
        vec![("react".to_owned(), seed.clone())],
        loader,
        Arc::new(ScriptedStyles::default()),
    );
    modules.register_static("app-shell", shell.clone()).unwrap();
    modules.prefetch(&ClientModuleId::new("b")).await.unwrap();
    let value = modules.import("a").await.unwrap();
    assert_eq!(value["b"], "from-b");
    assert!(Arc::ptr_eq(&modules.import("react").await.unwrap(), &seed));
    assert!(Arc::ptr_eq(
        &modules.import("app-shell").await.unwrap(),
        &shell
    ));
    assert!(
        modules.cache_snapshot()[&ClientModuleId::new("a")]
            .edges
            .contains("b/client")
    );

    let missed = Arc::new(ScriptedLoader::default());
    missed.bundle("a", Arc::new(|require| require.require("ghost")));
    let missed = system(
        vec![row("a")],
        Vec::new(),
        missed,
        Arc::new(ScriptedStyles::default()),
    );
    assert!(
        missed
            .import("a")
            .await
            .unwrap_err()
            .to_string()
            .contains("require(\"ghost\") missed")
    );

    let cyclic = Arc::new(ScriptedLoader::default());
    cyclic.bundle("a", Arc::new(|require| require.require("b")));
    cyclic.bundle("b", Arc::new(|require| require.require("a")));
    let cycle = system(
        vec![row("a"), row("b")],
        Vec::new(),
        cyclic,
        Arc::new(ScriptedStyles::default()),
    );
    cycle.prefetch(&ClientModuleId::new("b")).await.unwrap();
    assert!(
        cycle
            .import("a")
            .await
            .unwrap_err()
            .to_string()
            .contains("require cycle through \"a\"")
    );
}

#[tokio::test]
async fn failures_are_loud_and_invalidate_forces_a_fresh_generation() {
    let loader = Arc::new(ScriptedLoader::default());
    loader.missing("missing");
    let modules = system(
        vec![row("missing")],
        Vec::new(),
        loader,
        Arc::new(ScriptedStyles::default()),
    );
    assert!(
        modules
            .import("missing")
            .await
            .unwrap_err()
            .to_string()
            .contains("loaded without registering \"missing\"")
    );
    assert!(
        modules
            .import("unknown")
            .await
            .unwrap_err()
            .to_string()
            .contains("cannot resolve \"unknown\"")
    );
    assert!(
        modules
            .prefetch(&ClientModuleId::new("unknown"))
            .await
            .unwrap_err()
            .to_string()
            .contains("prefetch(\"unknown\")")
    );
    assert!(
        ClientModuleSystem::new(
            vec![row("a"), row("a")],
            Vec::<(String, Exports)>::new(),
            Arc::new(ScriptedLoader::default()),
            Arc::new(ScriptedStyles::default()),
        )
        .unwrap_err()
        .to_string()
        .contains("duplicate graph entry \"a\"")
    );

    let loader = Arc::new(ScriptedLoader::default());
    let generation = Arc::new(AtomicUsize::new(0));
    loader.bundle("a", {
        let generation = generation.clone();
        Arc::new(move |_| {
            Ok(exports(json!({
                "generation": generation.fetch_add(1, Ordering::AcqRel) + 1
            })))
        })
    });
    let modules = system(
        vec![row("a")],
        Vec::new(),
        loader.clone(),
        Arc::new(ScriptedStyles::default()),
    );
    let first = modules.import("a").await.unwrap();
    modules.invalidate(&ClientModuleId::new("a"));
    modules.prefetch(&ClientModuleId::new("a")).await.unwrap();
    let second = modules.import("a").await.unwrap();
    assert_eq!(first["generation"], 1);
    assert_eq!(second["generation"], 2);
    assert_eq!(loader.fetched.lock().len(), 2);
}

#[tokio::test]
async fn duplicate_registration_static_registration_and_style_inventory_match() {
    let loader = Arc::new(ScriptedLoader::default());
    let styles = Arc::new(ScriptedStyles::default());
    styles.owned.lock().insert(
        ClientModuleId::new("a"),
        vec!["a".to_owned(), "sheet-1".to_owned()],
    );
    loader.bundle("a", Arc::new(|_| Ok(exports(json!({})))));
    let modules = system(vec![row("a")], Vec::new(), loader, styles);
    let registrar = modules.registrar();
    registrar
        .register(
            ClientModuleId::new("x"),
            Arc::new(|_| Ok(exports(json!({})))),
        )
        .unwrap();
    assert!(
        registrar
            .register(
                ClientModuleId::new("x"),
                Arc::new(|_| Ok(exports(json!({})))),
            )
            .unwrap_err()
            .to_string()
            .contains("duplicate factory registration")
    );
    modules
        .register_static("shell", exports(json!({})))
        .unwrap();
    assert!(
        modules
            .register_static("shell", exports(json!({})))
            .unwrap_err()
            .to_string()
            .contains("registered twice")
    );
    modules.import("a").await.unwrap();
    assert_eq!(
        modules.cache_snapshot()[&ClientModuleId::new("a")].styles,
        ["a", "sheet-1"]
    );
}
