//! Downstream-style built artifact smoke for the public LSP stdio API.

use std::sync::Arc;

use seekdeep_cordis::Context;
use seekdeep_fs_local::{Config as FsConfig, LocalFileSystem};
use seekdeep_lsp::{Lsp, LspOperation, LspPosition, LspQueryRequest};
use seekdeep_lsp_stdio::plugin;
use seekdeep_subprocess_local::LocalSubprocessRuntime;
use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let fixture = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("fixture executable argument is required"))?;
    let root = tempfile::tempdir()?;
    let canonical = tokio::fs::canonicalize(root.path()).await?;
    let workspace = canonical.join("ws");
    tokio::fs::create_dir(&workspace).await?;
    tokio::fs::write(workspace.join("a.ts"), "const x = 1\n").await?;
    let source_uri = url::Url::from_file_path(workspace.join("a.ts"))
        .map_err(|()| anyhow::anyhow!("workspace source has no file URL"))?
        .to_string();

    let context = Context::new();
    let lsp = Arc::new(Lsp::new());
    lsp.provide(&context)?;
    LocalSubprocessRuntime::install(&context)?;
    LocalFileSystem::install(
        &context,
        FsConfig {
            cwd: Some(canonical.to_string_lossy().into_owned()),
            diff_basis_max_bytes: None,
        },
    )?;
    let fiber = context.plugin(
        plugin(),
        json!({"servers": {"fake": {
            "command": fixture,
            "env": {"LSP_FAKE_DEF": json!({
                "uri": source_uri,
                "range": {
                    "start": {"line": 0, "character": 0},
                    "end": {"line": 0, "character": 3}
                }
            }).to_string()},
            "extensionToLanguage": {".ts": "typescript"}
        }}}),
    )?;
    fiber.await_settled().await?;
    let result = lsp
        .query(
            LspQueryRequest {
                operation: LspOperation::GoToDefinition,
                file_path: "a.ts".to_owned(),
                position: LspPosition {
                    line: 0.0,
                    character: 6.0,
                },
                workspace_root: workspace.to_string_lossy().into_owned(),
            },
            None,
        )
        .await?;
    println!("{}", serde_json::to_string(&result)?);
    context.fiber().restart().await?;
    Ok(())
}
