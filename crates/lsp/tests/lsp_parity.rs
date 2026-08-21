//! Provider registry and normalized semantic-query parity.

use std::sync::Arc;

use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_cordis::{Context, Fiber};
use seekdeep_llm::AbortSignal;
use seekdeep_lsp::{
    LSP, LSP_CONFLICT, LSP_INVALID_PROVIDER, LSP_UNAVAILABLE, Lsp, LspError, LspHover,
    LspOperation, LspPosition, LspProvider, LspProviderId, LspProviderQuery, LspQueryRequest,
    LspQueryResult, final_extension, install,
};

#[derive(Debug)]
struct Provider {
    id: LspProviderId,
    mappings: IndexMap<String, String>,
    result: LspQueryResult,
    seen: Mutex<Vec<LspProviderQuery>>,
    signals: Mutex<Vec<Option<AbortSignal>>>,
}

#[async_trait::async_trait]
impl LspProvider for Provider {
    fn id(&self) -> &LspProviderId {
        &self.id
    }

    fn extension_to_language(&self) -> &IndexMap<String, String> {
        &self.mappings
    }

    async fn query(
        &self,
        request: LspProviderQuery,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<LspQueryResult> {
        self.seen.lock().push(request);
        self.signals.lock().push(signal);
        Ok(self.result.clone())
    }
}

fn provider(id: &str, mappings: &[(&str, &str)]) -> Arc<Provider> {
    Arc::new(Provider {
        id: LspProviderId::new(id),
        mappings: mappings
            .iter()
            .map(|(extension, language)| ((*extension).to_owned(), (*language).to_owned()))
            .collect(),
        result: LspQueryResult::Locations {
            locations: Vec::new(),
            resolved_workspace_uri: "file:///ws".to_owned(),
        },
        seen: Mutex::new(Vec::new()),
        signals: Mutex::new(Vec::new()),
    })
}

fn query(path: &str, operation: LspOperation) -> LspQueryRequest {
    LspQueryRequest {
        operation,
        file_path: path.to_owned(),
        position: LspPosition {
            line: 0.0,
            character: 0.0,
        },
        workspace_root: "/ws".to_owned(),
    }
}

fn error_code(error: &anyhow::Error) -> &'static str {
    error.downcast_ref::<LspError>().unwrap().code()
}

#[test]
fn final_extension_is_cross_platform_final_lowercase_and_dotfile_safe() {
    for (path, expected) in [
        ("src/Foo.TS", ".ts"),
        ("a/b/foo.d.ts", ".ts"),
        (r"C:\proj\Main.CS", ".cs"),
        ("Makefile", ""),
        (".bashrc", ""),
        ("dir.d/file", ""),
    ] {
        assert_eq!(final_extension(path), expected);
    }
}

#[tokio::test]
async fn registration_validation_conflicts_and_disposal_are_atomic() {
    let context = Context::new();
    let lsp = Arc::new(Lsp::new());
    for (candidate, expected) in [
        (
            provider("  ", &[(".ts", "typescript")]),
            LSP_INVALID_PROVIDER,
        ),
        (provider("empty", &[]), LSP_INVALID_PROVIDER),
        (
            provider("archive", &[(".tar.gz", "archive")]),
            LSP_INVALID_PROVIDER,
        ),
        (provider("language", &[(".ts", "  ")]), LSP_INVALID_PROVIDER),
        (
            provider("duplicate", &[(".ts", "typescript"), ("TS", "ts2")]),
            LSP_INVALID_PROVIDER,
        ),
    ] {
        let candidate: Arc<dyn LspProvider> = candidate;
        let error = lsp.register_provider(&context, candidate).unwrap_err();
        assert_eq!(error_code(&error), expected);
    }

    let first = provider("ts", &[(".ts", "typescript")]);
    let first_registration = lsp.register_provider(&context, first.clone()).unwrap();
    let duplicate_id: Arc<dyn LspProvider> = provider("ts", &[(".tsx", "tsx")]);
    assert_eq!(
        error_code(&lsp.register_provider(&context, duplicate_id).unwrap_err()),
        LSP_CONFLICT
    );
    let duplicate_extension: Arc<dyn LspProvider> = provider("other", &[("TS", "other")]);
    assert_eq!(
        error_code(
            &lsp.register_provider(&context, duplicate_extension)
                .unwrap_err()
        ),
        LSP_CONFLICT
    );
    let partial: Arc<dyn LspProvider> = provider("py-ts", &[(".py", "python"), (".ts", "x")]);
    assert_eq!(
        error_code(&lsp.register_provider(&context, partial).unwrap_err()),
        LSP_CONFLICT
    );
    assert_eq!(
        error_code(
            &lsp.query(query("a.py", LspOperation::GoToDefinition), None)
                .await
                .unwrap_err()
        ),
        LSP_UNAVAILABLE
    );

    first_registration.dispose().await.unwrap();
    assert_eq!(
        error_code(
            &lsp.query(query("a.ts", LspOperation::GoToDefinition), None)
                .await
                .unwrap_err()
        ),
        LSP_UNAVAILABLE
    );
    lsp.register_provider(&context, provider("ts", &[(".ts", "typescript")]))
        .unwrap();
}

#[tokio::test]
async fn query_selection_language_signal_and_owner_lifecycle_are_exact() {
    let context = Context::new();
    let mounted = install(&context).unwrap();
    mounted.await_settled().await.unwrap();
    let lsp = context.get(LSP).unwrap();
    let owner_fiber = Fiber::active_child("provider-owner");
    let owner = context.with_fiber(owner_fiber.clone());
    let ts = provider("ts", &[("TS", "typescript")]);
    let py = provider("py", &[(".py", "python")]);
    lsp.register_provider(&owner, ts.clone()).unwrap();
    lsp.register_provider(&context, py.clone()).unwrap();
    let signal = AbortSignal::default();
    assert_eq!(
        lsp.query(query("a.ts", LspOperation::Hover), Some(signal.clone()))
            .await
            .unwrap(),
        LspQueryResult::Locations {
            locations: Vec::new(),
            resolved_workspace_uri: "file:///ws".to_owned(),
        }
    );
    {
        let seen = ts.seen.lock();
        assert_eq!(seen[0].language_id, "typescript");
        assert_eq!(seen[0].operation, LspOperation::Hover);
    }
    signal.abort();
    assert!(ts.signals.lock()[0].as_ref().unwrap().is_aborted());
    assert!(
        lsp.query(query("a.py", LspOperation::FindReferences), None)
            .await
            .is_ok()
    );

    owner_fiber.dispose().await.unwrap();
    assert_eq!(
        error_code(
            &lsp.query(query("a.ts", LspOperation::GoToDefinition), None)
                .await
                .unwrap_err()
        ),
        LSP_UNAVAILABLE
    );
    mounted.dispose().await.unwrap();
    assert!(context.get(LSP).is_none());
}

#[test]
fn result_union_is_closed_and_nullable_hover_is_lossless() {
    let hover = LspQueryResult::Hover {
        hover: Some(LspHover {
            contents: "details".to_owned(),
            range: None,
        }),
    };
    let value = serde_json::to_value(&hover).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"kind": "hover", "hover": {"contents": "details"}})
    );
    assert_eq!(
        serde_json::from_value::<LspQueryResult>(value).unwrap(),
        hover
    );
    assert!(
        serde_json::from_value::<LspQueryResult>(
            serde_json::json!({"kind": "symbols", "symbols": []})
        )
        .is_err()
    );
}
