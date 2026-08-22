//! Native graph watches, dirty recovery, SSE, config, and teardown parity.

use std::{fs, path::PathBuf, sync::Arc, time::Duration};

use seekdeep_client_hmr::*;
use seekdeep_client_modules::{
    ClientHostEntry, ClientModuleHost, ClientModuleId, FilesystemClientPackageResolver,
};
use seekdeep_cordis::Context;
use seekdeep_host_webserver::{ListenHost, WebServer, WebServerConfig};
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

struct Fixture {
    root: TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            root: tempfile::tempdir().unwrap(),
        }
    }

    fn package(&self, name: &str, body: &str) -> PathBuf {
        let root = name
            .split('/')
            .fold(self.root.path().join("node_modules"), |path, part| {
                path.join(part)
            });
        fs::create_dir_all(root.join("lib")).unwrap();
        fs::write(
            root.join("package.json"),
            serde_json::to_vec(&serde_json::json!({
                "name": name,
                "exports": {"./client": "./lib/client.js"},
                "seekdeep": {"client": {"platform": "web"}},
            }))
            .unwrap(),
        )
        .unwrap();
        let bundle = root.join("lib/client.js");
        fs::write(&bundle, body).unwrap();
        bundle
    }

    fn host(&self, names: &[&str]) -> Arc<ClientModuleHost> {
        ClientModuleHost::new(
            Arc::new(FilesystemClientPackageResolver::new(self.root.path())),
            &names
                .iter()
                .map(|name| ClientHostEntry {
                    name: ClientModuleId::new(*name),
                    mounted: true,
                    disabled: false,
                })
                .collect::<Vec<_>>(),
            Arc::new(|_| {}),
        )
        .unwrap()
    }
}

async fn server() -> (Context, Arc<WebServer>) {
    let context = Context::new();
    let server = WebServer::install(
        &context,
        WebServerConfig {
            host: ListenHost::Loopback,
            port: 0,
        },
    )
    .await
    .unwrap();
    (context, server)
}

#[tokio::test]
async fn watches_graph_changes_recovers_vanished_bundles_and_stops_on_dispose() {
    let fixture = Fixture::new();
    let early = fixture.package("pkg-early", "v1");
    let host = fixture.host(&["pkg-early"]);
    let (context, web_server) = server().await;
    let service = ClientHmrHostService::start(
        host.clone(),
        &web_server,
        &ClientHmrConfig {
            poll_interval_ms: 60_000,
        },
    )
    .unwrap();
    assert_eq!(service.watched_count(), 1);

    let before = host.graph().rev.clone();
    fs::write(&early, "v2-longer").unwrap();
    service.poll_now();
    assert_ne!(host.graph().rev, before);

    let late = fixture.package("pkg-late", "v1");
    host.reconcile(
        ClientModuleId::new("pkg-late"),
        &[
            ClientHostEntry {
                name: ClientModuleId::new("pkg-early"),
                mounted: true,
                disabled: false,
            },
            ClientHostEntry {
                name: ClientModuleId::new("pkg-late"),
                mounted: true,
                disabled: false,
            },
        ],
    );
    assert_eq!(service.watched_count(), 2);
    let before_late = host.graph().rev.clone();
    fs::write(&late, "v2-late-longer").unwrap();
    service.poll_now();
    assert_ne!(host.graph().rev, before_late);

    fs::remove_file(&late).unwrap();
    service.poll_now();
    fs::write(&late, "replacement").unwrap();
    let before_recovery = host.graph().rev.clone();
    service.poll_now();
    assert_ne!(host.graph().rev, before_recovery);

    host.reconcile(
        ClientModuleId::new("pkg-late"),
        &[ClientHostEntry {
            name: ClientModuleId::new("pkg-early"),
            mounted: true,
            disabled: false,
        }],
    );
    assert_eq!(service.watched_count(), 1);
    service.dispose().await.unwrap();
    assert_eq!(service.watched_count(), 0);
    context.root_fiber().restart().await.unwrap();
}

#[tokio::test]
async fn sse_sends_graph_and_rebuilt_frames_and_route_is_owned() {
    let fixture = Fixture::new();
    let bundle = fixture.package("pkg-a", "v1");
    let host = fixture.host(&["pkg-a"]);
    let (context, web_server) = server().await;
    let service = ClientHmrHostService::start(
        host,
        &web_server,
        &ClientHmrConfig {
            poll_interval_ms: 60_000,
        },
    )
    .unwrap();
    let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", web_server.port()))
        .await
        .unwrap();
    socket
        .write_all(
            b"GET /plugins/events HTTP/1.1\r\nHost: localhost\r\nConnection: keep-alive\r\n\r\n",
        )
        .await
        .unwrap();
    let mut bytes = vec![0_u8; 8192];
    let read = tokio::time::timeout(Duration::from_secs(2), socket.read(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    let opened = String::from_utf8_lossy(&bytes[..read]);
    assert!(opened.contains("text/event-stream"));
    assert!(opened.contains(": connected"));
    assert!(opened.contains("\"type\":\"graph\""));

    fs::write(bundle, "v2-longer").unwrap();
    service.poll_now();
    let read = tokio::time::timeout(Duration::from_secs(2), socket.read(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    let rebuilt = String::from_utf8_lossy(&bytes[..read]);
    assert!(rebuilt.contains("\"type\":\"rebuilt\""));
    assert!(rebuilt.contains("\"id\":\"pkg-a\""));

    service.dispose().await.unwrap();
    let mut replacement = tokio::net::TcpStream::connect(("127.0.0.1", web_server.port()))
        .await
        .unwrap();
    replacement
        .write_all(b"GET /plugins/events HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    let read = tokio::time::timeout(Duration::from_secs(2), replacement.read(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&bytes[..read]).contains("404"));
    context.root_fiber().restart().await.unwrap();
}

#[tokio::test]
async fn config_plugin_and_invariant_are_strict_and_reversible() {
    let plugin = client_hmr_host_plugin();
    assert_eq!(plugin.inject(), ["clientModules", "webServer"]);
    let context = Context::new();
    let registry = Arc::new(InvariantRegistry::new(&context, &InvariantConfig::default()).unwrap());
    let registration = register_client_hmr_invariant(&registry).unwrap();
    assert!(register_client_hmr_invariant(&registry).is_err());
    registration.dispose().await.unwrap();
    assert!(
        serde_json::from_value::<ClientHmrConfig>(serde_json::json!({
            "pollIntervalMs": 20
        }))
        .is_ok()
    );
    assert!(
        serde_json::from_value::<ClientHmrConfig>(serde_json::json!({
            "unknown": true
        }))
        .is_err()
    );
}
