//! Host Client-package graph composition, bundle serving, and index injection.

use std::{
    collections::HashMap,
    convert::Infallible,
    fs, io,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    sync::Arc,
};

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::{Method, Response, StatusCode, header};
use indexmap::{IndexMap, IndexSet};
use parking_lot::RwLock;
use percent_encoding::percent_decode_str;
use seekdeep_cordis::{Context, EventOptions, Plugin, PluginFiber, ServiceKey};
use seekdeep_host_webserver::{WEB_SERVER, WebBody, WebHandler, WebRoute, WebRouteKind, WebServer};
use seekdeep_loader::{LOADER, LoaderEntrySnapshot};
use serde_json::Value;
use sha1::{Digest as _, Sha1};

use crate::{ClientModuleId, WebBootEntry, WebBootGraph};

/// Typed Host service corresponding to `ctx.clientModules`.
pub const CLIENT_MODULES: ServiceKey<ClientModuleHost> = ServiceKey::new("clientModules");
/// Stable Host plugin name.
pub const CLIENT_MODULES_NAME: &str = "client-modules";

/// Host Cordis plugin that derives package resolution from the Loader base URL.
#[must_use]
pub fn client_modules_host_plugin() -> Plugin {
    Plugin::new(
        CLIENT_MODULES_NAME,
        ["loader", "webServer"],
        |context, _| {
            Box::pin(async move {
                let base_url = context
                    .meta("loader.base_url")
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "client-modules: loader.base_url is unset — the Host half needs the config-tree anchor to resolve plugin packages"
                        )
                    })?;
                let base_dir = url::Url::parse(&base_url)
                    .ok()
                    .and_then(|url| url.to_file_path().ok())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "client-modules: loader.base_url must be a file URL, received {base_url:?}"
                        )
                    })?;
                install_client_module_host(&context, base_dir)?;
                Ok(())
            })
        },
    )
}

/// Enabled Loader row relevant to Client package composition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientHostEntry {
    /// Configured package specifier.
    pub name: ClientModuleId,
    /// Whether a live Fiber exists.
    pub mounted: bool,
    /// Effective disabled state.
    pub disabled: bool,
}

/// Package metadata read once per configured name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientPackageMetadata {
    /// Built Client artifact path.
    pub client_path: PathBuf,
    /// Informational dependency edges.
    pub inject: Option<Vec<String>>,
    /// Stage-one prefetch marker.
    pub immediately: bool,
}

/// Package metadata resolver behind the Host graph.
pub trait ClientPackageResolver: Send + Sync + 'static {
    /// Resolves a package declaration, or `None` for non-Client rows.
    ///
    /// # Errors
    ///
    /// Returns package metadata read, parse, or declaration failures.
    fn resolve(&self, package: &ClientModuleId) -> anyhow::Result<Option<ClientPackageMetadata>>;
}

/// Node-compatible `node_modules` resolver rooted at the Loader config directory.
#[derive(Clone, Debug)]
pub struct FilesystemClientPackageResolver {
    base_dir: PathBuf,
}

impl FilesystemClientPackageResolver {
    /// Creates a resolver that walks `base_dir` and its ancestors.
    #[must_use]
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
        }
    }

    fn package_json(&self, package: &ClientModuleId) -> Option<PathBuf> {
        let package_path = package
            .as_str()
            .split('/')
            .fold(PathBuf::new(), |path, part| path.join(part));
        self.base_dir.ancestors().find_map(|ancestor| {
            let candidate = ancestor
                .join("node_modules")
                .join(&package_path)
                .join("package.json");
            candidate.exists().then_some(candidate)
        })
    }
}

impl ClientPackageResolver for FilesystemClientPackageResolver {
    fn resolve(&self, package: &ClientModuleId) -> anyhow::Result<Option<ClientPackageMetadata>> {
        let Some(package_json) = self.package_json(package) else {
            return Ok(None);
        };
        let parsed: Value = serde_json::from_slice(&fs::read(&package_json)?)?;
        let declaration = parsed
            .get("seekdeep")
            .and_then(|seekdeep| seekdeep.get("client"));
        let Some(declaration) = parse_client_declaration(package, declaration)? else {
            return Ok(None);
        };
        if declaration.platform != "web" {
            return Ok(None);
        }
        let client_export = client_export(package, parsed.get("exports"))?.ok_or_else(|| {
            anyhow::anyhow!(
                "client-modules: {package} declares seekdeep.client but exports no \"./client\" bundle"
            )
        })?;
        Ok(Some(ClientPackageMetadata {
            client_path: package_json
                .parent()
                .expect("package.json has a parent")
                .join(client_export.strip_prefix("./").unwrap_or(&client_export)),
            inject: declaration.inject,
            immediately: declaration.immediately,
        }))
    }
}

struct ClientDeclaration {
    platform: String,
    inject: Option<Vec<String>>,
    immediately: bool,
}

#[derive(Clone)]
struct WebPluginRecord {
    entry: WebBootEntry,
    client_path: PathBuf,
}

type RebuildListener = Arc<dyn Fn(ClientModuleId, String) + Send + Sync>;
type GraphListener = Arc<dyn Fn() + Send + Sync>;
type HostLogger = Arc<dyn Fn(String) + Send + Sync>;

struct HostState {
    table: IndexMap<ClientModuleId, WebPluginRecord>,
    metadata: HashMap<ClientModuleId, Option<ClientPackageMetadata>>,
    dirty: IndexSet<ClientModuleId>,
    graph: Arc<WebBootGraph>,
    rebuild_listeners: IndexMap<u64, RebuildListener>,
    graph_listeners: IndexMap<u64, GraphListener>,
    next_listener: u64,
}

/// Host-side Client package graph and bundle table.
pub struct ClientModuleHost {
    resolver: Arc<dyn ClientPackageResolver>,
    logger: HostLogger,
    state: RwLock<HostState>,
}

impl std::fmt::Debug for ClientModuleHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.read();
        formatter
            .debug_struct("ClientModuleHost")
            .field("entries", &state.table.len())
            .field("metadata", &state.metadata.len())
            .field("dirty", &state.dirty.len())
            .finish_non_exhaustive()
    }
}

impl ClientModuleHost {
    /// Performs the synchronous activation scan and groups every failure.
    ///
    /// # Errors
    ///
    /// Returns one grouped composition failure after reconciling all entries.
    pub fn new(
        resolver: Arc<dyn ClientPackageResolver>,
        entries: &[ClientHostEntry],
        logger: HostLogger,
    ) -> anyhow::Result<Arc<Self>> {
        let host = Arc::new(Self {
            resolver,
            logger,
            state: RwLock::new(HostState {
                table: IndexMap::new(),
                metadata: HashMap::new(),
                dirty: entries.iter().map(|entry| entry.name.clone()).collect(),
                graph: Arc::new(WebBootGraph {
                    rev: short_hash(b"[]"),
                    entries: Vec::new(),
                }),
                rebuild_listeners: IndexMap::new(),
                graph_listeners: IndexMap::new(),
                next_listener: 0,
            }),
        });
        let failures = host.flush(entries);
        if failures.is_empty() {
            Ok(host)
        } else {
            Err(anyhow::anyhow!(composition_failure(&failures)))
        }
    }

    /// Stable graph reference until the next table or revision change.
    #[must_use]
    pub fn graph(&self) -> Arc<WebBootGraph> {
        self.state.read().graph.clone()
    }

    /// Absolute bundle path for one known graph row.
    #[must_use]
    pub fn client_path(&self, id: &ClientModuleId) -> Option<PathBuf> {
        self.state
            .read()
            .table
            .get(id)
            .map(|record| record.client_path.clone())
    }

    /// Marks one Loader package dirty and reconciles all marked names.
    pub fn reconcile(&self, entry: ClientModuleId, entries: &[ClientHostEntry]) {
        self.state.write().dirty.insert(entry);
        for failure in self.flush(entries) {
            (self.logger)(failure.message);
        }
    }

    /// Re-hashes one bundle and notifies contained subscribers on change.
    ///
    /// # Errors
    ///
    /// Returns bundle read failures.
    pub fn rebuilt(&self, id: &ClientModuleId) -> anyhow::Result<Option<String>> {
        let (rev, listeners, graph_listeners) = {
            let mut state = self.state.write();
            let Some(record) = state.table.get_mut(id) else {
                return Ok(None);
            };
            let rev = short_hash(&fs::read(&record.client_path)?);
            if rev == record.entry.rev {
                return Ok(Some(rev));
            }
            record.entry = graph_row(
                id.clone(),
                rev.clone(),
                record.entry.inject.clone(),
                record.entry.immediately == Some(true),
            );
            recompose(&mut state);
            (
                rev,
                state
                    .rebuild_listeners
                    .values()
                    .cloned()
                    .collect::<Vec<_>>(),
                state.graph_listeners.values().cloned().collect::<Vec<_>>(),
            )
        };
        for listener in listeners {
            contain(&self.logger, || listener(id.clone(), rev.clone()));
        }
        for listener in graph_listeners {
            contain(&self.logger, || listener());
        }
        Ok(Some(rev))
    }

    /// Subscribes to changed bundle revisions.
    pub fn on_rebuilt(self: &Arc<Self>, listener: RebuildListener) -> HostSubscription {
        self.subscribe(ListenerKind::Rebuilt(listener))
    }

    /// Subscribes to graph changes.
    pub fn on_graph_changed(self: &Arc<Self>, listener: GraphListener) -> HostSubscription {
        self.subscribe(ListenerKind::Graph(listener))
    }

    /// Serves one bundle or source-map resource.
    pub fn serve(&self, method: &Method, pathname: &str) -> BundleResponse {
        if method != Method::GET && method != Method::HEAD {
            return BundleResponse::empty(StatusCode::METHOD_NOT_ALLOWED);
        }
        let decoded = percent_decode_str(pathname).decode_utf8_lossy();
        let prefix = "/plugins/";
        let map_suffix = "/client.js.map";
        let bundle_suffix = "/client.js";
        let is_map = decoded.starts_with(prefix) && decoded.ends_with(map_suffix);
        let suffix = if is_map { map_suffix } else { bundle_suffix };
        let Some(id) = decoded
            .strip_prefix(prefix)
            .and_then(|path| path.strip_suffix(suffix))
        else {
            return BundleResponse::empty(StatusCode::NOT_FOUND);
        };
        let Some(mut path) = self.client_path(&ClientModuleId::new(id)) else {
            return BundleResponse::empty(StatusCode::NOT_FOUND);
        };
        if is_map {
            path = PathBuf::from(format!("{}.map", path.display()));
        }
        match fs::read(path) {
            Ok(body) => BundleResponse {
                status: StatusCode::OK,
                content_type: Some(if is_map {
                    "application/json; charset=utf-8"
                } else {
                    "text/javascript; charset=utf-8"
                }),
                body,
            },
            Err(_) => BundleResponse::empty(StatusCode::NOT_FOUND),
        }
    }

    fn subscribe(self: &Arc<Self>, listener: ListenerKind) -> HostSubscription {
        let id = {
            let mut state = self.state.write();
            state.next_listener += 1;
            let id = state.next_listener;
            match listener {
                ListenerKind::Rebuilt(listener) => {
                    state.rebuild_listeners.insert(id, listener);
                }
                ListenerKind::Graph(listener) => {
                    state.graph_listeners.insert(id, listener);
                }
            }
            id
        };
        HostSubscription {
            host: Arc::downgrade(self),
            id,
        }
    }

    fn flush(&self, entries: &[ClientHostEntry]) -> Vec<CompositionFailure> {
        let dirty = std::mem::take(&mut self.state.write().dirty);
        let mut changed = false;
        let mut failures = Vec::new();
        for name in dirty {
            match self.process_one(&name, entries) {
                Ok(row_changed) => changed |= row_changed,
                Err(failure) => failures.push(failure),
            }
        }
        if changed {
            let listeners = {
                let mut state = self.state.write();
                recompose(&mut state);
                state.graph_listeners.values().cloned().collect::<Vec<_>>()
            };
            for listener in listeners {
                contain(&self.logger, || listener());
            }
        }
        failures
    }

    fn process_one(
        &self,
        name: &ClientModuleId,
        entries: &[ClientHostEntry],
    ) -> Result<bool, CompositionFailure> {
        let qualifies = entries
            .iter()
            .any(|entry| entry.name == *name && entry.mounted && !entry.disabled);
        if !qualifies {
            return Ok(self.state.write().table.shift_remove(name).is_some());
        }
        if self.state.read().table.contains_key(name) {
            return Ok(false);
        }
        let metadata = if let Some(cached) = self.state.read().metadata.get(name).cloned() {
            cached
        } else {
            let resolved = self
                .resolver
                .resolve(name)
                .map_err(|error| CompositionFailure::other(&error))?;
            self.state
                .write()
                .metadata
                .insert(name.clone(), resolved.clone());
            resolved
        };
        let Some(metadata) = metadata else {
            return Ok(false);
        };
        let bytes = fs::read(&metadata.client_path).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                CompositionFailure::missing(name.clone(), metadata.client_path.clone())
            } else {
                let failure = anyhow::anyhow!("{}: {}", io_error_label(&error), error);
                CompositionFailure::other(&failure)
            }
        })?;
        let rev = short_hash(&bytes);
        self.state.write().table.insert(
            name.clone(),
            WebPluginRecord {
                entry: graph_row(name.clone(), rev, metadata.inject, metadata.immediately),
                client_path: metadata.client_path,
            },
        );
        Ok(true)
    }
}

enum ListenerKind {
    Rebuilt(RebuildListener),
    Graph(GraphListener),
}

/// Exact-listener subscription disposer.
pub struct HostSubscription {
    host: std::sync::Weak<ClientModuleHost>,
    id: u64,
}

impl HostSubscription {
    /// Removes this exact listener idempotently.
    pub fn dispose(&self) {
        if let Some(host) = self.host.upgrade() {
            let mut state = host.state.write();
            state.rebuild_listeners.shift_remove(&self.id);
            state.graph_listeners.shift_remove(&self.id);
        }
    }
}

/// Core HTTP response used by the real `WebServer` adapter and tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleResponse {
    /// HTTP status.
    pub status: StatusCode,
    /// Content type for successful resources.
    pub content_type: Option<&'static str>,
    /// Complete body bytes.
    pub body: Vec<u8>,
}

impl BundleResponse {
    fn empty(status: StatusCode) -> Self {
        Self {
            status,
            content_type: None,
            body: Vec::new(),
        }
    }
}

/// Injects the boot graph before the shell reads it.
///
/// # Panics
///
/// Panics only if the statically serializable graph contract stops serializing.
#[must_use]
pub fn inject_boot_manifest(html: &str, graph: &WebBootGraph) -> String {
    let json = serde_json::to_string(graph)
        .expect("boot graph is serializable")
        .replace('<', "\\u003c");
    let script = format!("<script>window.__SEEKDEEP_BOOT__ = {json}</script>");
    html.find("<head>").map_or_else(
        || format!("{script}{html}"),
        |head| {
            let after = head + "<head>".len();
            format!("{}{script}{}", &html[..after], &html[after..])
        },
    )
}

/// Installs the real Host graph service, bundle route, and index tap.
///
/// # Errors
///
/// Returns missing Host services, invalid base URL, composition, route, or
/// Cordis ownership failures.
pub fn install_client_module_host(
    context: &Context,
    base_dir: impl Into<PathBuf>,
) -> anyhow::Result<Arc<ClientModuleHost>> {
    let loader = context
        .get(LOADER)
        .ok_or_else(|| anyhow::anyhow!("client-modules requires loader"))?;
    let web_server = context
        .get(WEB_SERVER)
        .ok_or_else(|| anyhow::anyhow!("client-modules requires webServer"))?;
    let snapshots = loader.entries()?;
    let entries = host_entries(&snapshots);
    let host = ClientModuleHost::new(
        Arc::new(FilesystemClientPackageResolver::new(base_dir)),
        &entries,
        Arc::new(|message| tracing::warn!(%message, "client module composition warning")),
    )?;
    context.provide(CLIENT_MODULES, host.clone())?;
    register_web_faces(context, &web_server, &host)?;
    let event_host = host.clone();
    let event_loader = loader.clone();
    context.events().on(
        context,
        "internal/plugin",
        move |_, args| {
            let host = event_host.clone();
            let loader = event_loader.clone();
            let entry_name = args
                .get::<PluginFiber>(0)
                .and_then(|fiber| fiber.entry_name())
                .map(ClientModuleId::new);
            Box::pin(async move {
                let Some(entry_name) = entry_name else {
                    return Ok(seekdeep_cordis::EventReply::Undefined);
                };
                let _ = loader.wait().await;
                if let Ok(snapshots) = loader.entries() {
                    host.reconcile(entry_name, &host_entries(&snapshots));
                }
                Ok(seekdeep_cordis::EventReply::Undefined)
            })
        },
        EventOptions::default(),
    )?;
    Ok(host)
}

fn register_web_faces(
    context: &Context,
    web_server: &WebServer,
    host: &Arc<ClientModuleHost>,
) -> anyhow::Result<()> {
    let route_host = host.clone();
    let route: WebHandler = Arc::new(move |request| {
        let response = route_host.serve(request.method(), request.uri().path());
        Box::pin(async move { Ok(web_response(response)) })
    });
    let route = web_server.register(WebRoute {
        kind: WebRouteKind::Prefix,
        path: "/plugins".to_owned(),
        handler: route,
    })?;
    let graph_host = host.clone();
    let tap = web_server.tap_index(Arc::new(move |html| {
        inject_boot_manifest(&html, &graph_host.graph())
    }));
    route.own(context, "client-modules: bundle route")?;
    tap.own(context, "client-modules: boot manifest injection")?;
    Ok(())
}

fn web_response(response: BundleResponse) -> seekdeep_host_webserver::WebResponse {
    let mut builder = Response::builder().status(response.status);
    if let Some(content_type) = response.content_type {
        builder = builder
            .header(header::CONTENT_TYPE, content_type)
            .header(header::CACHE_CONTROL, "no-cache");
    }
    builder
        .body(full(response.body))
        .expect("valid bundle response")
}

fn full(body: Vec<u8>) -> WebBody {
    Full::new(Bytes::from(body))
        .map_err(|never: Infallible| match never {})
        .boxed_unsync()
}

fn host_entries(snapshots: &[LoaderEntrySnapshot]) -> Vec<ClientHostEntry> {
    snapshots
        .iter()
        .filter(|entry| !entry.group)
        .map(|entry| ClientHostEntry {
            name: ClientModuleId::new(entry.plugin.as_str()),
            mounted: entry.state.is_some(),
            disabled: entry.disabled,
        })
        .collect()
}

fn parse_client_declaration(
    package: &ClientModuleId,
    value: Option<&Value>,
) -> anyhow::Result<Option<ClientDeclaration>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let object = value.as_object().ok_or_else(|| {
        anyhow::anyhow!("client-modules: {package} has a non-object seekdeep.client declaration")
    })?;
    let platform = object
        .get("platform")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            anyhow::anyhow!("client-modules: {package} seekdeep.client.platform must be a string")
        })?
        .to_owned();
    let inject = object
        .get("inject")
        .map(|value| {
            value
                .as_array()
                .and_then(|values| {
                    values
                        .iter()
                        .map(|value| value.as_str().map(str::to_owned))
                        .collect::<Option<Vec<_>>>()
                })
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "client-modules: {package} seekdeep.client.inject must be a string array"
                    )
                })
        })
        .transpose()?;
    let immediately = object
        .get("immediately")
        .map(|value| {
            value.as_bool().ok_or_else(|| {
                anyhow::anyhow!(
                    "client-modules: {package} seekdeep.client.immediately must be a boolean"
                )
            })
        })
        .transpose()?
        .unwrap_or(false);
    Ok(Some(ClientDeclaration {
        platform,
        inject,
        immediately,
    }))
}

fn client_export(
    package: &ClientModuleId,
    exports: Option<&Value>,
) -> anyhow::Result<Option<String>> {
    let Some(client) = exports
        .and_then(Value::as_object)
        .and_then(|exports| exports.get("./client"))
    else {
        return Ok(None);
    };
    if let Some(path) = client.as_str() {
        return Ok(Some(path.to_owned()));
    }
    if let Some(path) = client
        .as_object()
        .and_then(|conditions| conditions.get("default"))
        .and_then(Value::as_str)
    {
        return Ok(Some(path.to_owned()));
    }
    anyhow::bail!(
        "client-modules: {package} exports[\"./client\"] must be a string or an object with a string default"
    )
}

fn graph_row(
    id: ClientModuleId,
    rev: String,
    inject: Option<Vec<String>>,
    immediately: bool,
) -> WebBootEntry {
    WebBootEntry {
        url: format!("/plugins/{id}/client.js?rev={rev}"),
        id,
        rev,
        inject,
        immediately: immediately.then_some(true),
    }
}

fn recompose(state: &mut HostState) {
    let entries = state
        .table
        .values()
        .map(|record| record.entry.clone())
        .collect::<Vec<_>>();
    state.graph = Arc::new(WebBootGraph {
        rev: short_hash(&serde_json::to_vec(&entries).expect("entries serialize")),
        entries,
    });
}

fn short_hash(bytes: &[u8]) -> String {
    let digest = Sha1::digest(bytes);
    hex::encode(digest)[..12].to_owned()
}

fn contain(logger: &HostLogger, callback: impl FnOnce()) {
    if catch_unwind(AssertUnwindSafe(callback)).is_err() {
        logger("client-modules subscriber panicked".to_owned());
    }
}

struct CompositionFailure {
    kind: FailureKind,
    message: String,
}

enum FailureKind {
    Missing {
        package: ClientModuleId,
        path: PathBuf,
    },
    Other,
}

impl CompositionFailure {
    fn missing(package: ClientModuleId, path: PathBuf) -> Self {
        Self {
            kind: FailureKind::Missing { package, path },
            message: "client bundle not found".to_owned(),
        }
    }

    fn other(error: &anyhow::Error) -> Self {
        Self {
            kind: FailureKind::Other,
            message: format!("{error:#}"),
        }
    }
}

fn composition_failure(failures: &[CompositionFailure]) -> String {
    let noun = if failures.len() == 1 {
        "package"
    } else {
        "packages"
    };
    let mut lines = vec![format!(
        "client-modules: {} client {noun} failed to compose:",
        failures.len()
    )];
    let missing = failures
        .iter()
        .filter_map(|failure| match &failure.kind {
            FailureKind::Missing { package, path } => Some((package, path)),
            FailureKind::Other => None,
        })
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        lines.push("  client bundles not found; run `pnpm run build` before launch:".to_owned());
        for (package, path) in missing {
            lines.push(format!("    - package: {package}"));
            lines.push(format!("      path: {}", path.display()));
        }
    }
    let other = failures
        .iter()
        .filter(|failure| matches!(&failure.kind, FailureKind::Other))
        .collect::<Vec<_>>();
    if !other.is_empty() {
        lines.push("  other failures:".to_owned());
        lines.extend(
            other
                .into_iter()
                .map(|failure| format!("    - {}", failure.message)),
        );
    }
    lines.join("\n")
}

fn io_error_label(error: &io::Error) -> &'static str {
    if error.kind() == io::ErrorKind::IsADirectory {
        "EISDIR"
    } else {
        "I/O error"
    }
}
