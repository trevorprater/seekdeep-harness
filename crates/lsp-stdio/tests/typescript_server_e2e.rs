//! Keyless real TypeScript language-server compatibility floor.

use std::{path::PathBuf, sync::Arc, time::Duration};

use seekdeep_cordis::Context;
use seekdeep_fs_local::{Config as FsConfig, LocalFileSystem};
use seekdeep_lsp::{Lsp, LspOperation, LspPosition, LspQueryRequest, LspQueryResult};
use seekdeep_lsp_stdio::plugin;
use seekdeep_subprocess_local::LocalSubprocessRuntime;
use serde_json::json;

fn source_server_binary() -> PathBuf {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    workspace
        .parent()
        .and_then(std::path::Path::parent)
        .and_then(std::path::Path::parent)
        .expect("workspace parent")
        .join(
            "deepseek-harness/packages/lsp/lsp-stdio/node_modules/.bin/typescript-language-server",
        )
}

fn request(
    workspace: &std::path::Path,
    operation: LspOperation,
    line_one_based: u32,
    character_one_based: u32,
) -> LspQueryRequest {
    LspQueryRequest {
        operation,
        file_path: "shapes.ts".to_owned(),
        position: LspPosition {
            line: f64::from(line_one_based - 1),
            character: f64::from(character_one_based - 1),
        },
        workspace_root: workspace.to_string_lossy().into_owned(),
    }
}

async fn mount_server() -> (tempfile::TempDir, PathBuf, Context, Arc<Lsp>) {
    let server = source_server_binary();
    assert!(server.is_file(), "missing {}", server.display());
    let root = tempfile::tempdir().unwrap();
    let canonical = tokio::fs::canonicalize(root.path()).await.unwrap();
    let workspace = canonical.join("proj");
    tokio::fs::create_dir(&workspace).await.unwrap();
    tokio::fs::write(
        workspace.join("tsconfig.json"),
        json!({"compilerOptions": {"strict": true, "module": "nodenext"}}).to_string(),
    )
    .await
    .unwrap();
    tokio::fs::write(
        workspace.join("shapes.ts"),
        [
            "export interface Shape {",
            "  area(): number",
            "}",
            "",
            "export class Circle implements Shape {",
            "  constructor(private r: number) {}",
            "  area(): number { return Math.PI * this.r * this.r }",
            "}",
            "",
            "export function describe(s: Shape): string {",
            "  return `area=${s.area()}`",
            "}",
            "",
            "const c = new Circle(2)",
            "export const text = describe(c)",
            "",
        ]
        .join("\n"),
    )
    .await
    .unwrap();
    let context = Context::new();
    let lsp = Arc::new(Lsp::new());
    lsp.provide(&context).unwrap();
    LocalSubprocessRuntime::install(&context).unwrap();
    LocalFileSystem::install(
        &context,
        FsConfig {
            cwd: Some(canonical.to_string_lossy().into_owned()),
            diff_basis_max_bytes: None,
        },
    )
    .unwrap();
    let fiber = context
        .plugin(
            plugin(),
            json!({"servers": {"typescript": {
                "command": server,
                "args": ["--stdio"],
                "extensionToLanguage": {
                    ".ts": "typescript",
                    ".tsx": "typescriptreact"
                }
            }}}),
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(30), fiber.await_settled())
        .await
        .expect("provider setup timed out")
        .unwrap();
    (root, workspace, context, lsp)
}

#[tokio::test]
async fn real_typescript_server_supports_all_four_semantic_operations() {
    let (_root, workspace, context, lsp) = mount_server().await;

    let definition = tokio::time::timeout(
        Duration::from_secs(30),
        lsp.query(
            request(&workspace, LspOperation::GoToDefinition, 15, 22),
            None,
        ),
    )
    .await
    .expect("definition timed out")
    .unwrap();
    assert!(matches!(
        definition,
        LspQueryResult::Locations { ref locations, .. }
            if !locations.is_empty() && locations.iter().any(|location| location.uri.ends_with("shapes.ts"))
    ));

    let references = lsp
        .query(
            request(&workspace, LspOperation::FindReferences, 10, 17),
            None,
        )
        .await
        .unwrap();
    assert!(matches!(
        references,
        LspQueryResult::Locations { ref locations, .. } if locations.len() >= 2
    ));

    let implementations = lsp
        .query(
            request(&workspace, LspOperation::GoToImplementation, 1, 18),
            None,
        )
        .await
        .unwrap();
    assert!(matches!(
        implementations,
        LspQueryResult::Locations { ref locations, .. } if !locations.is_empty()
    ));

    let hover = lsp
        .query(request(&workspace, LspOperation::Hover, 14, 15), None)
        .await
        .unwrap();
    assert!(matches!(
        hover,
        LspQueryResult::Hover { hover: Some(ref hover) } if hover.contents.contains("Circle")
    ));

    tokio::time::timeout(Duration::from_secs(10), context.fiber().restart())
        .await
        .expect("real server teardown timed out")
        .unwrap();
}
