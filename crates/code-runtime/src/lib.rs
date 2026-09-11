//! Service seam for hostile program execution against async host bindings.

use std::{
    collections::HashSet,
    fmt,
    future::Future,
    pin::Pin,
    sync::{Arc, LazyLock},
};

use async_trait::async_trait;
use indexmap::IndexMap;
use seekdeep_cordis::{Context, ServiceKey, fiber::EffectHandle};
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use seekdeep_llm::AbortSignal;
use serde::{Deserialize, Serialize};

pub mod json;
pub use json::{CodeJsonString, CodeJsonValue};

/// Async host binding resolution.
pub type CodeBindingFuture =
    Pin<Box<dyn Future<Output = anyhow::Result<CodeJsonValue>> + Send + 'static>>;
/// One host-side function exposed to a program.
pub type CodeBindingFunction =
    Arc<dyn Fn(CodeJsonValue) -> CodeBindingFuture + Send + Sync + 'static>;

/// A host binding rejection whose program-visible message may contain lone
/// UTF-16 surrogates. Backends retain `message` when transporting this error.
#[derive(Debug)]
pub struct CodeBindingFailure {
    /// The exact program-visible rejection message.
    pub message: CodeJsonString,
}

impl fmt::Display for CodeBindingFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(
            self.message
                .as_str()
                .unwrap_or_else(|| self.message.as_raw()),
        )
    }
}

impl std::error::Error for CodeBindingFailure {}

/// Program-visible typed rejection contract for one namespace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodeBindingErrorClass {
    /// Constructor global and resulting error name.
    pub name: String,
    /// Own property carrying the rejected member name.
    pub member_name_property: CodeJsonString,
}

/// Named async functions exposed under one program global.
#[derive(Clone)]
pub struct CodeBindingNamespace {
    /// Portable global identifier.
    pub global: String,
    /// Exact callable members in declaration order.
    pub functions: IndexMap<CodeJsonString, CodeBindingFunction>,
    /// Optional typed member-rejection contract.
    pub error_class: Option<CodeBindingErrorClass>,
}

impl fmt::Debug for CodeBindingNamespace {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodeBindingNamespace")
            .field("global", &self.global)
            .field("functions", &self.functions.keys().collect::<Vec<_>>())
            .field("error_class", &self.error_class)
            .finish()
    }
}

/// Complete input for one isolated program run.
#[derive(Clone, Debug)]
pub struct CodeRunRequest {
    /// Program body in the backend's declared language.
    pub program: CodeJsonString,
    /// Host namespaces exposed to the program.
    pub bindings: Vec<CodeBindingNamespace>,
    /// Optional caller cancellation.
    pub signal: Option<AbortSignal>,
}

/// Orthogonal run-failure taxonomy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodeRunFailureKind {
    /// Program parse, transform, or execution exception.
    Exception,
    /// Implementation-owned time budget expired.
    Timeout,
    /// Caller cancellation fired.
    Abort,
    /// Execution substrate died without settling.
    WorkerExit,
    /// Completion value was not lossless JSON.
    InvalidOutput,
    /// Bounded serialized logs/value/diagnostic exceeded the cap.
    OutputLimit,
}

/// One resolved program failure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeRunFailure {
    /// Failure class.
    pub kind: CodeRunFailureKind,
    /// Human-readable self-correction detail.
    pub message: CodeJsonString,
}

/// Resolved outcome of one run.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeRunResult {
    /// Lossless JSON completion, if the program returned one.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "json::deserialize_optional"
    )]
    pub value: Option<CodeJsonValue>,
    /// Ordered captured log text.
    pub logs: Vec<CodeJsonString>,
    /// Present exactly when the run failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<CodeRunFailure>,
}

/// Runtime implementation contract.
#[async_trait]
pub trait CodeRuntimeBackend: Send + Sync + 'static {
    /// Lowercase source-language descriptor.
    fn language(&self) -> &str;
    /// Lowercase isolation-substrate descriptor.
    fn isolation(&self) -> &str;
    /// Runs one program; contract misuse rejects, program failures resolve.
    async fn run(&self, request: CodeRunRequest) -> anyhow::Result<CodeRunResult>;
}

/// Lifecycle-owned `ctx.codeRuntime` service.
#[derive(Clone)]
pub struct CodeRuntime {
    backend: Arc<dyn CodeRuntimeBackend>,
}

impl fmt::Debug for CodeRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodeRuntime")
            .field("language", &self.language())
            .field("isolation", &self.isolation())
            .finish_non_exhaustive()
    }
}

impl CodeRuntime {
    /// Wraps one runtime implementation.
    #[must_use]
    pub fn new(backend: Arc<dyn CodeRuntimeBackend>) -> Self {
        Self { backend }
    }

    /// Backend source-language descriptor.
    #[must_use]
    pub fn language(&self) -> &str {
        self.backend.language()
    }

    /// Backend isolation descriptor.
    #[must_use]
    pub fn isolation(&self) -> &str {
        self.backend.isolation()
    }

    /// Executes a request through the implementation.
    ///
    /// # Errors
    ///
    /// Returns only service-contract misuse; program and substrate failures are
    /// represented by [`CodeRunResult::error`].
    pub async fn run(&self, request: CodeRunRequest) -> anyhow::Result<CodeRunResult> {
        self.backend.run(request).await
    }

    /// Provides this runtime on `ctx.codeRuntime` for the mounting fiber.
    ///
    /// # Errors
    ///
    /// Returns standard duplicate-service or inactive-owner failures.
    pub fn provide(
        self: &Arc<Self>,
        context: &Context,
    ) -> Result<EffectHandle, seekdeep_cordis::CordisError> {
        context.provide(CODE_RUNTIME, self.clone())
    }
}

/// Typed Cordis slot corresponding to `ctx.codeRuntime`.
pub const CODE_RUNTIME: ServiceKey<CodeRuntime> = ServiceKey::new("codeRuntime");

/// Backend-owned global names refused by every implementation.
pub static RESERVED_BINDING_GLOBALS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    HashSet::from([
        "console",
        "__dsh_main__",
        "__builtins__",
        "__name__",
        "__debug__",
    ])
});

/// Error and exception protocol member names refused by every implementation.
pub static RESERVED_ERROR_MEMBERS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    HashSet::from([
        "name",
        "message",
        "stack",
        "args",
        "with_traceback",
        "add_note",
    ])
});

/// ECMAScript and Python reserved-word union for portable globals.
pub static PORTABLE_RESERVED_WORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    HashSet::from([
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "debugger",
        "default",
        "delete",
        "do",
        "else",
        "enum",
        "export",
        "extends",
        "false",
        "finally",
        "for",
        "function",
        "if",
        "import",
        "in",
        "instanceof",
        "new",
        "null",
        "return",
        "super",
        "switch",
        "this",
        "throw",
        "true",
        "try",
        "typeof",
        "var",
        "void",
        "while",
        "with",
        "yield",
        "let",
        "static",
        "implements",
        "interface",
        "package",
        "private",
        "protected",
        "public",
        "arguments",
        "eval",
        "False",
        "None",
        "True",
        "and",
        "as",
        "assert",
        "async",
        "def",
        "del",
        "elif",
        "except",
        "from",
        "global",
        "is",
        "lambda",
        "nonlocal",
        "not",
        "or",
        "pass",
        "raise",
        "match",
        "type",
        "_",
    ])
});

/// Whether a member has Python dunder form (`__x__`, non-empty middle).
#[must_use]
pub fn is_dunder_member(name: &str) -> bool {
    name.len() > 4 && name.starts_with("__") && name.ends_with("__")
        && !name.contains(['\n', '\r', '\u{2028}', '\u{2029}'])
}

/// Matches the source's dunder expression on exact JavaScript string code units.
#[must_use]
pub fn is_dunder_member_string(name: &CodeJsonString) -> bool {
    let units = name.utf16_units();
    units.len() > 4 && units.starts_with(&[95, 95]) && units.ends_with(&[95, 95])
        && !units.iter().any(|unit| matches!(unit, 10 | 13 | 0x2028 | 0x2029))
}

/// Registers the seam's explained empty invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register("seekdeep-code-runtime", InvariantInstaller::noop())
}

#[cfg(test)]
mod tests {
    use parking_lot::Mutex;
    use serde_json::Value;

    use super::*;

    struct StubRuntime {
        requests: Mutex<Vec<CodeJsonString>>,
        next: Mutex<CodeRunResult>,
    }

    #[async_trait]
    impl CodeRuntimeBackend for StubRuntime {
        fn language(&self) -> &'static str {
            "typescript"
        }

        fn isolation(&self) -> &'static str {
            "in-process-stub"
        }

        async fn run(&self, request: CodeRunRequest) -> anyhow::Result<CodeRunResult> {
            self.requests.lock().push(request.program);
            if request.signal.as_ref().is_some_and(AbortSignal::is_aborted) {
                return Ok(CodeRunResult {
                    error: Some(CodeRunFailure {
                        kind: CodeRunFailureKind::Abort,
                        message: request
                            .signal
                            .as_ref()
                            .and_then(AbortSignal::reason)
                            .map_or_else(|| "undefined".to_owned(), |reason| reason.to_string())
                            .into(),
                    }),
                    ..CodeRunResult::default()
                });
            }
            for namespace in request.bindings {
                for function in namespace.functions.values() {
                    function(serde_json::json!({ "from": "stub" }).into()).await?;
                }
            }
            Ok(self.next.lock().clone())
        }
    }

    fn backend() -> Arc<StubRuntime> {
        Arc::new(StubRuntime {
            requests: Mutex::new(Vec::new()),
            next: Mutex::new(CodeRunResult::default()),
        })
    }

    #[tokio::test]
    async fn service_registers_runs_reports_failures_and_disposes() {
        let context = Context::new();
        let implementation = backend();
        let runtime = Arc::new(CodeRuntime::new(implementation.clone()));
        let effect = runtime.provide(&context).unwrap();
        assert_eq!(context.get(CODE_RUNTIME).unwrap().language(), "typescript");
        assert_eq!(runtime.isolation(), "in-process-stub");

        let calls = Arc::new(Mutex::new(Vec::new()));
        let observed = calls.clone();
        let result = runtime
            .run(CodeRunRequest {
                program: "return 1".into(),
                bindings: vec![CodeBindingNamespace {
                    global: "tools".to_owned(),
                    functions: IndexMap::from_iter([(
                        "probe".into(),
                        Arc::new(move |arguments| {
                            observed.lock().push(arguments);
                            Box::pin(async { Ok(Value::Null.into()) }) as CodeBindingFuture
                        }) as CodeBindingFunction,
                    )]),
                    error_class: None,
                }],
                signal: None,
            })
            .await
            .unwrap();
        assert_eq!(result, CodeRunResult::default());
        assert_eq!(
            calls.lock().as_slice(),
            &[serde_json::json!({ "from": "stub" })]
        );
        assert_eq!(implementation.requests.lock().as_slice(), &["return 1"]);

        *implementation.next.lock() = CodeRunResult {
            logs: vec!["boom".into()],
            error: Some(CodeRunFailure {
                kind: CodeRunFailureKind::Exception,
                message: "boom".into(),
            }),
            ..CodeRunResult::default()
        };
        let failed = runtime
            .run(CodeRunRequest {
                program: "throw".into(),
                bindings: Vec::new(),
                signal: None,
            })
            .await
            .unwrap();
        assert_eq!(failed.error.unwrap().kind, CodeRunFailureKind::Exception);

        let duplicate = Arc::new(CodeRuntime::new(backend()));
        assert!(duplicate.provide(&context).is_err());
        effect.dispose().await.unwrap();
        assert!(context.get(CODE_RUNTIME).is_none());
    }

    #[tokio::test]
    async fn preaborted_signal_resolves_as_abort() {
        let implementation = backend();
        let runtime = CodeRuntime::new(implementation);
        let signal = AbortSignal::default();
        signal.abort_with_reason(serde_json::json!("cancelled"));
        let result = runtime
            .run(CodeRunRequest {
                program: "return 1".into(),
                bindings: Vec::new(),
                signal: Some(signal),
            })
            .await
            .unwrap();
        assert_eq!(
            result.error,
            Some(CodeRunFailure {
                kind: CodeRunFailureKind::Abort,
                message: "\"cancelled\"".into(),
            })
        );
    }

    #[test]
    fn portable_exclusion_sets_and_dunder_rule_are_exact() {
        for name in [
            "console",
            "__dsh_main__",
            "__builtins__",
            "__name__",
            "__debug__",
        ] {
            assert!(RESERVED_BINDING_GLOBALS.contains(name));
        }
        assert!(!RESERVED_BINDING_GLOBALS.contains("tools"));
        for name in [
            "name",
            "message",
            "stack",
            "args",
            "with_traceback",
            "add_note",
        ] {
            assert!(RESERVED_ERROR_MEMBERS.contains(name));
        }
        assert!(!RESERVED_ERROR_MEMBERS.contains("code"));
        for name in ["__dict__", "__init__", "__x__"] {
            assert!(is_dunder_member(name));
        }
        for name in ["_private", "name", "__mid", "__", "____", "__\n__", "__\r__", "__\u{2028}__", "__\u{2029}__", "__x__\n"] {
            assert!(!is_dunder_member(name));
        }
        assert!(is_dunder_member_string(&CodeJsonString::from_utf16(&[95, 95, 0xd800, 95, 95])));
        for name in ["function", "lambda", "nonlocal", "class"] {
            assert!(PORTABLE_RESERVED_WORDS.contains(name));
        }
        assert!(!PORTABLE_RESERVED_WORDS.contains("tools"));
    }

    #[test]
    fn result_wire_shape_and_invariant_identity_are_exact() {
        let result = CodeRunResult {
            value: Some(Value::Null.into()),
            logs: vec!["line".into()],
            error: Some(CodeRunFailure {
                kind: CodeRunFailureKind::WorkerExit,
                message: "gone".into(),
            }),
        };
        assert_eq!(
            serde_json::to_value(result).unwrap(),
            serde_json::json!({
                "value": null,
                "logs": ["line"],
                "error": { "kind": "worker-exit", "message": "gone" }
            })
        );
        let context = Context::new();
        let registry = Arc::new(
            InvariantRegistry::new(&context, &seekdeep_invariants::InvariantConfig::default())
                .unwrap(),
        );
        let _registration = register_invariant(&registry).unwrap();
        assert!(registry.is_registered("seekdeep-code-runtime"));
    }
}
