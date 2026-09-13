//! Client closure redirects, Host calls, console mirroring, plugin shape, and styles.

use std::{
    collections::BTreeSet,
    sync::{Arc, Weak},
};

use futures::future::BoxFuture;
use parking_lot::Mutex;
use seekdeep_cordis_dynamic_types::CordisDynamicPluginId;
use serde_json::Value;

const TIMER_REDIRECT: &str = "browser timer globals are unavailable in dynamic packages. Declare inject: ['timer'] on the returned plugin, query Client Service.listService for the exact API, and close over that plugin ctx. In React, create timers from an event handler or React.useEffect and return callback-form disposers from the effect cleanup.";
const FETCH_REDIRECT: &str = "network belongs to the HOST half: register a handler there with harness.handle(method, fn) and call it here via host.call(method, args).";
const REQUIRE_REDIRECT: &str = "modules cannot be imported here. React arrives as the `React` closure symbol; everything else goes through ctx services or host.call.";

/// One Client closure symbol exposed through Inspect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClientBuiltinInspection {
    /// Exact closure symbol.
    pub name: &'static str,
    /// Capability and restriction summary.
    pub description: &'static str,
    /// Model-visible call signatures.
    pub signatures: &'static [&'static str],
}

/// Exact Client Builtin directory owned beside the evaluator.
pub const CLIENT_BUILTIN_INSPECTION: &[ClientBuiltinInspection] = &[
    ClientBuiltinInspection {
        name: "ctx",
        description: "Restricted Cordis Context. Prefer ctx.get(name) with an undefined check; use inject only for hard dependencies.",
        signatures: &[
            "ctx.get(name: string): unknown | undefined",
            "ctx.on(name: string, listener: Function): () => void",
            "ctx.provide(name: string, value: unknown): () => void",
            "ctx.effect(callback: Function, label?: string): () => void",
        ],
    },
    ClientBuiltinInspection {
        name: "React",
        description: "React runtime exposed without JSX transformation.",
        signatures: &[
            "React.createElement(type, props, ...children): ReactElement",
            "React.useState(initial)",
            "React.useEffect(effect, deps)",
        ],
    },
    ClientBuiltinInspection {
        name: "host",
        description: "Package-private JSON RPC from Client to this Package's Host half.",
        signatures: &["host.call(method: string, args?: JsonValue): Promise<JsonValue>"],
    },
    ClientBuiltinInspection {
        name: "styles",
        description: "Package-owned stylesheet insertion cleaned up with the Client run.",
        signatures: &["styles.insert(css: string): () => void"],
    },
    ClientBuiltinInspection {
        name: "console",
        description: "Package-tagged browser logging.",
        signatures: &[
            "console.log(...values): void",
            "console.error(...values): void",
        ],
    },
];

/// Withheld browser globals and their exact teaching redirects.
pub const DYNAMIC_CLIENT_REDIRECTS: &[(&str, &str)] = &[
    ("setTimeout", TIMER_REDIRECT),
    ("setInterval", TIMER_REDIRECT),
    ("clearTimeout", TIMER_REDIRECT),
    ("clearInterval", TIMER_REDIRECT),
    ("fetch", FETCH_REDIRECT),
    ("require", REQUIRE_REDIRECT),
];

/// Returns the exact teaching failure for one shadowed browser global.
#[must_use]
pub fn client_redirect_failure(name: &str) -> Option<String> {
    DYNAMIC_CLIENT_REDIRECTS
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map(|(_, redirect)| {
            format!("{name} is not available in a dynamic client half — {redirect}")
        })
}

/// Returns the Host/Client split diagnostic for any `harness` property.
#[must_use]
pub fn harness_split_failure(property: &str) -> String {
    format!(
        "harness.{property} belongs to the HOST half (`code`): register handlers there with harness.handle(method, fn); the browser half calls them via host.call(method, args)."
    )
}

/// Browser-engine parse fallback after Host syntax precheck accepted the source.
#[must_use]
pub fn client_parse_failure(message: &str) -> String {
    format!(
        "client half failed to parse in this browser: {message}\nThe browser half is plain JavaScript (no JSX, no TypeScript); build elements with React.createElement."
    )
}

/// Teaching error for an absent closure return.
pub const CLIENT_MISSING_RETURN: &str = "client half returned `undefined` — did you forget `return`?\n  ✓ return (ctx) => { … }\n  ✓ return { name: '…', inject: ['slots'], apply(ctx) { … } }";

/// Teaching error for a non-mountable closure return.
pub const CLIENT_INVALID_PLUGIN: &str =
    "client half must `return` a plugin: a function, or an object with an `apply(ctx)` method";

/// Mountable shape returned by the browser engine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvaluatedClientPlugin {
    /// Function-form plugin with no declared Services.
    Function,
    /// Object-form plugin and its declared Services.
    Object {
        /// Exact declaration order.
        inject: Vec<String>,
    },
}

/// Browser-engine value shape used for mountability classification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientPluginCandidate {
    /// JavaScript `undefined`.
    Undefined,
    /// Callable function.
    Function,
    /// Object with or without callable `apply`.
    Object {
        /// Whether `apply` is callable.
        has_apply: bool,
        /// Declared Service names when mountable.
        inject: Vec<String>,
    },
    /// Any non-object, non-function value.
    Other,
}

/// Narrows an engine return to the two mountable plugin forms.
///
/// # Errors
///
/// Returns the exact missing-return or invalid-plugin teaching error.
pub fn classify_client_plugin(
    candidate: ClientPluginCandidate,
) -> Result<EvaluatedClientPlugin, &'static str> {
    match candidate {
        ClientPluginCandidate::Function => Ok(EvaluatedClientPlugin::Function),
        ClientPluginCandidate::Object {
            has_apply: true,
            inject,
        } => Ok(EvaluatedClientPlugin::Object { inject }),
        ClientPluginCandidate::Undefined => Err(CLIENT_MISSING_RETURN),
        ClientPluginCandidate::Object {
            has_apply: false, ..
        }
        | ClientPluginCandidate::Other => Err(CLIENT_INVALID_PLUGIN),
    }
}

/// One console argument after browser-side shape classification.
#[derive(Clone, Debug, PartialEq)]
pub enum ClientConsoleArgument {
    /// Error message without its stack envelope.
    Error(String),
    /// Plain string.
    String(String),
    /// JavaScript `undefined`.
    Undefined,
    /// Lossless JSON value.
    Json(Value),
    /// Circular, exotic, or otherwise non-serializable value.
    Unserializable,
}

/// Mirrors one `console.error` line into the load report, bounded to 500 UTF-16 units.
#[must_use]
pub fn mirror_console_error(arguments: &[ClientConsoleArgument]) -> String {
    let text = arguments
        .iter()
        .map(|argument| match argument {
            ClientConsoleArgument::Error(message) | ClientConsoleArgument::String(message) => {
                message.clone()
            }
            ClientConsoleArgument::Undefined => "undefined".to_owned(),
            ClientConsoleArgument::Json(value) => serde_json::to_string(value)
                .unwrap_or_else(|_| "[unserializable console argument]".to_owned()),
            ClientConsoleArgument::Unserializable => "[unserializable console argument]".to_owned(),
        })
        .collect::<Vec<_>>()
        .join(" ");
    String::from_utf16_lossy(&text.encode_utf16().take(500).collect::<Vec<_>>())
}

/// Rust-owned Host invocation seam supplied to one Client package.
pub type ClientHostInvoke =
    Arc<dyn Fn(String, Value) -> BoxFuture<'static, anyhow::Result<Value>> + Send + Sync>;

/// `host.call` binding scoped to one exact package Run.
#[derive(Clone)]
pub struct ClientHost {
    invoke: ClientHostInvoke,
}

impl std::fmt::Debug for ClientHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("ClientHost").finish_non_exhaustive()
    }
}

impl ClientHost {
    /// Creates a Host binding over the Runner invoke seam.
    #[must_use]
    pub fn new(invoke: ClientHostInvoke) -> Self {
        Self { invoke }
    }

    /// Calls one private Host method; omitted arguments cross the JSON wire as `null`.
    ///
    /// # Errors
    ///
    /// Returns the Runner invocation or transport failure unchanged.
    pub async fn call(
        &self,
        method: impl Into<String>,
        args: Option<Value>,
    ) -> anyhow::Result<Value> {
        (self.invoke)(method.into(), args.unwrap_or(Value::Null)).await
    }
}

/// Opaque browser style node identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StyleTagId(u64);

impl StyleTagId {
    /// Wraps a DOM adapter-owned identity.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
}

/// Browser DOM operations used by dynamic stylesheet ownership.
pub trait StyleDom: Send + Sync + 'static {
    /// Inserts one `<style data-dyn=plugin_id>` node.
    ///
    /// # Errors
    ///
    /// Returns DOM insertion failures.
    fn insert(&self, plugin_id: &CordisDynamicPluginId, css: &str) -> anyhow::Result<StyleTagId>;
    /// Removes one node idempotently.
    fn remove(&self, tag: StyleTagId);
}

struct StyleState {
    dom: Arc<dyn StyleDom>,
    tags: Mutex<BTreeSet<StyleTagId>>,
}

/// Per-package stylesheet bookkeeping owned by the Client Run.
pub struct DynamicCordisStyles {
    plugin_id: CordisDynamicPluginId,
    state: Arc<StyleState>,
}

impl std::fmt::Debug for DynamicCordisStyles {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DynamicCordisStyles")
            .field("plugin_id", &self.plugin_id)
            .field("count", &self.count())
            .finish_non_exhaustive()
    }
}

impl DynamicCordisStyles {
    /// Creates empty style ownership for one stable Plugin.
    #[must_use]
    pub fn new(plugin_id: CordisDynamicPluginId, dom: Arc<dyn StyleDom>) -> Self {
        Self {
            plugin_id,
            state: Arc::new(StyleState {
                dom,
                tags: Mutex::new(BTreeSet::new()),
            }),
        }
    }

    /// Inserts a CSS string and returns an early disposer.
    ///
    /// # Errors
    ///
    /// Rejects a non-string binding value or DOM insertion failure.
    pub fn insert_value(&self, css: &Value) -> anyhow::Result<StyleDisposer> {
        let css = css
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("styles.insert(css) needs a CSS string"))?;
        self.insert(css)
    }

    /// Inserts a CSS string and returns an early disposer.
    ///
    /// # Errors
    ///
    /// Returns DOM insertion failures.
    pub fn insert(&self, css: &str) -> anyhow::Result<StyleDisposer> {
        let tag = self.state.dom.insert(&self.plugin_id, css)?;
        self.state.tags.lock().insert(tag);
        Ok(StyleDisposer {
            state: Arc::downgrade(&self.state),
            tag,
        })
    }

    /// Number of live style tags owned by this package.
    #[must_use]
    pub fn count(&self) -> usize {
        self.state.tags.lock().len()
    }

    /// Removes every still-live package style.
    pub fn dispose(&self) {
        let tags = std::mem::take(&mut *self.state.tags.lock());
        for tag in tags {
            self.state.dom.remove(tag);
        }
    }
}

/// Idempotent early disposer for one dynamic style tag.
pub struct StyleDisposer {
    state: Weak<StyleState>,
    tag: StyleTagId,
}

impl StyleDisposer {
    /// Removes this tag while leaving sibling styles intact.
    pub fn dispose(&self) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        if state.tags.lock().remove(&self.tag) {
            state.dom.remove(self.tag);
        }
    }
}
