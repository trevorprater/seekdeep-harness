//! Host package composition, diagnostics, graph, route, and index parity.

use std::{fs, path::PathBuf, sync::Arc};

use parking_lot::Mutex;
use seekdeep_client_modules::*;
use seekdeep_cordis::Context;
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use tempfile::TempDir;

struct Fixture {
    root: TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            root: tempfile::tempdir().unwrap(),
        }
    }

    fn write_package(&self, name: &str, declaration: &serde_json::Value) -> PathBuf {
        let package = name
            .split('/')
            .fold(self.root.path().join("node_modules"), |path, part| {
                path.join(part)
            });
        fs::create_dir_all(&package).unwrap();
        fs::write(
            package.join("package.json"),
            serde_json::to_vec(&serde_json::json!({
                "name": name,
                "exports": {
                    "./client": "./lib/client.js",
                    "./package.json": "./package.json"
                },
                "seekdeep": declaration,
            }))
            .unwrap(),
        )
        .unwrap();
        package.join("lib/client.js")
    }

    fn resolver(&self) -> Arc<FilesystemClientPackageResolver> {
        Arc::new(FilesystemClientPackageResolver::new(self.root.path()))
    }
}

fn entry(name: &str) -> ClientHostEntry {
    ClientHostEntry {
        name: ClientModuleId::new(name),
        mounted: true,
        disabled: false,
    }
}

type TestLogger = Arc<dyn Fn(String) + Send + Sync>;

fn logger() -> (Arc<Mutex<Vec<String>>>, TestLogger) {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let observed = messages.clone();
    (
        messages,
        Arc::new(move |message| observed.lock().push(message)),
    )
}

#[test]
fn sibling_seekdeep_roles_compose_one_client_graph_row() {
    assert_eq!(
        client_modules_host_plugin().inject(),
        ["loader", "webServer"]
    );
    let fixture = Fixture::new();
    let name = "@fixture/current-client-field";
    let path = fixture.write_package(
        name,
        &serde_json::json!({
            "bundle": {"patch": "./cordis.patch.yml"},
            "client": {"platform": "web"},
            "profile": {"bundles": []}
        }),
    );
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "module.exports = {}\n").unwrap();
    let (_, logger) = logger();
    let host = ClientModuleHost::new(fixture.resolver(), &[entry(name)], logger).unwrap();
    assert_eq!(host.graph().entries.len(), 1);
    assert_eq!(host.graph().entries[0].id.as_str(), name);
    assert_eq!(host.client_path(&ClientModuleId::new(name)), Some(path));
}

#[test]
fn missing_bundles_group_under_one_build_instruction() {
    let fixture = Fixture::new();
    let first = "@fixture/missing-first";
    let second = "@fixture/missing-second";
    let first_path =
        fixture.write_package(first, &serde_json::json!({"client": {"platform": "web"}}));
    let second_path =
        fixture.write_package(second, &serde_json::json!({"client": {"platform": "web"}}));
    let (_, logger) = logger();
    let error = ClientModuleHost::new(fixture.resolver(), &[entry(first), entry(second)], logger)
        .unwrap_err()
        .to_string();
    for expected in [
        "client-modules: 2 client packages failed to compose:",
        "client bundles not found; run `pnpm run build` before launch:",
        &format!("    - package: {first}"),
        &format!("      path: {}", first_path.display()),
        &format!("    - package: {second}"),
        &format!("      path: {}", second_path.display()),
    ] {
        assert!(error.contains(expected), "{error}");
    }
}

#[test]
fn non_missing_bundle_errors_stay_in_the_other_failure_group() {
    let fixture = Fixture::new();
    let name = "@fixture/unreadable-client";
    let path = fixture.write_package(name, &serde_json::json!({"client": {"platform": "web"}}));
    fs::create_dir_all(&path).unwrap();
    let (_, logger) = logger();
    let error = ClientModuleHost::new(fixture.resolver(), &[entry(name)], logger)
        .unwrap_err()
        .to_string();
    assert!(error.contains("client-modules: 1 client package failed to compose:"));
    assert!(error.contains("other failures:"));
    assert!(error.contains("EISDIR"));
    assert!(!error.contains("pnpm run build"));
}

#[test]
fn bundle_route_serves_source_maps_and_rejects_other_methods_or_paths() {
    let fixture = Fixture::new();
    let name = "@fixture/source-map";
    let path = fixture.write_package(name, &serde_json::json!({"client": {"platform": "web"}}));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "module.exports = {}\n").unwrap();
    let source_map = b"{\"version\":3,\"sources\":[\"src/client/index.tsx\"]}\n";
    fs::write(format!("{}.map", path.display()), source_map).unwrap();
    let (_, logger) = logger();
    let host = ClientModuleHost::new(fixture.resolver(), &[entry(name)], logger).unwrap();
    let response = host.serve(
        &hyper::Method::GET,
        &format!("/plugins/{name}/client.js.map"),
    );
    assert_eq!(response.status, hyper::StatusCode::OK);
    assert_eq!(
        response.content_type,
        Some("application/json; charset=utf-8")
    );
    assert_eq!(response.body, source_map);
    assert_eq!(
        host.serve(&hyper::Method::POST, "/plugins/x/client.js")
            .status,
        hyper::StatusCode::METHOD_NOT_ALLOWED
    );
    assert_eq!(
        host.serve(&hyper::Method::GET, "/plugins/unknown/client.js")
            .status,
        hyper::StatusCode::NOT_FOUND
    );
}

#[test]
fn rebuild_reconcile_subscriptions_and_manifest_injection_are_stable_and_contained() {
    let fixture = Fixture::new();
    let name = "@fixture/rebuild";
    let path = fixture.write_package(
        name,
        &serde_json::json!({
            "client": {"platform": "web", "inject": ["a"], "immediately": true}
        }),
    );
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "first").unwrap();
    let (logs, logger) = logger();
    let host = ClientModuleHost::new(fixture.resolver(), &[entry(name)], logger).unwrap();
    let first = host.graph();
    assert!(Arc::ptr_eq(&first, &host.graph()));
    assert_eq!(
        first.entries[0].inject.as_deref(),
        Some(&["a".to_owned()][..])
    );
    assert_eq!(first.entries[0].immediately, Some(true));

    let rebuilds = Arc::new(Mutex::new(Vec::new()));
    let observed = rebuilds.clone();
    let _good = host.on_rebuilt(Arc::new(move |id, rev| {
        observed.lock().push((id, rev));
    }));
    let _bad = host.on_rebuilt(Arc::new(|_, _| panic!("subscriber failed")));
    fs::write(&path, "second").unwrap();
    let rev = host.rebuilt(&ClientModuleId::new(name)).unwrap().unwrap();
    assert_eq!(rev.len(), 12);
    assert_eq!(rebuilds.lock().len(), 1);
    assert_eq!(
        logs.lock().as_slice(),
        &["client-modules subscriber panicked"]
    );
    assert!(!Arc::ptr_eq(&first, &host.graph()));

    let html = inject_boot_manifest(
        "<html><head><script src=app.js></script></head></html>",
        &WebBootGraph {
            rev: "x<y".to_owned(),
            entries: host.graph().entries.clone(),
        },
    );
    assert!(html.contains("<head><script>window.__SEEKDEEP_BOOT__ = "));
    assert!(html.contains("x\\u003cy"));

    host.reconcile(
        ClientModuleId::new(name),
        &[ClientHostEntry {
            name: ClientModuleId::new(name),
            mounted: false,
            disabled: false,
        }],
    );
    assert!(host.graph().entries.is_empty());
}

#[tokio::test]
async fn invariant_reserves_the_atomic_graph_path_relation() {
    let registry =
        Arc::new(InvariantRegistry::new(&Context::new(), &InvariantConfig::default()).unwrap());
    let registration = register_client_modules_invariant(&registry).unwrap();
    assert!(register_client_modules_invariant(&registry).is_err());
    registration.dispose().await.unwrap();
    register_client_modules_invariant(&registry)
        .unwrap()
        .dispose()
        .await
        .unwrap();
}
