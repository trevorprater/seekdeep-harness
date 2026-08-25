//! One-turn minimal agent client backed by the compiled Rust JSON-RPC runtime.

use std::{collections::BTreeMap, path::PathBuf, process::ExitCode};

use clap::Parser;
use seekdeep_core::session::SessionId;
use seekdeep_sdk_client::{
    DeepSeekHarness, DeepSeekHarnessOptions, HarnessClientOptions, RunOptions,
};

const RUNTIME_MARKER: &str = "--seekdeep-runtime";
const CONFIG: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../examples/jsonrpc-agent/minimal.cordis.yml"
);

#[derive(Debug, Parser)]
#[command(name = "seekdeep-jsonrpc-minimal")]
struct Arguments {
    /// Task for the minimal agent.
    prompt: String,
    /// Agent and runtime working directory.
    #[arg(long, default_value = ".")]
    workspace: PathBuf,
    /// Durable session-log directory.
    #[arg(long, default_value = ".seekdeep-sessions")]
    session_root: PathBuf,
    /// Exact session identity.
    #[arg(long)]
    session_id: Option<String>,
    /// Provider route.
    #[arg(long, default_value = "deepseek-official")]
    provider: String,
    /// Model route.
    #[arg(long, env = "SEEKDEEP_MODEL", default_value = "deepseek-v4-flash")]
    model: String,
    /// Optional output-token cap.
    #[arg(long)]
    max_tokens: Option<u64>,
}

fn main() -> ExitCode {
    if std::env::args().nth(1).as_deref() == Some(RUNTIME_MARKER) {
        return seekdeep_sdk_jsonrpc_demo::runner::process_main(false);
    }
    let arguments = Arguments::parse();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("seekdeep-jsonrpc-minimal: {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(arguments)) {
        Ok(response) => {
            println!("{response}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(arguments: Arguments) -> anyhow::Result<String> {
    let workspace = std::fs::canonicalize(&arguments.workspace)?;
    let session_root = absolute(&arguments.session_root)?;
    let mut environment = std::env::vars().collect::<BTreeMap<_, _>>();
    environment.insert("SEEKDEEP_CORDIS_CONFIG".to_owned(), CONFIG.to_owned());
    environment.insert(
        "SEEKDEEP_CWD".to_owned(),
        workspace.to_string_lossy().into_owned(),
    );
    environment.insert(
        "SEEKDEEP_SESSION_ROOT".to_owned(),
        session_root.to_string_lossy().into_owned(),
    );
    let mut launch =
        HarnessClientOptions::new(std::env::current_exe()?.to_string_lossy().into_owned());
    launch.args = vec![RUNTIME_MARKER.to_owned()];
    launch.cwd = Some(workspace.to_string_lossy().into_owned());
    launch.env = Some(environment);
    let harness = DeepSeekHarness::new(DeepSeekHarnessOptions {
        launch,
        cwd: Some(workspace.to_string_lossy().into_owned()),
        provider: Some(arguments.provider),
        model: Some(arguments.model),
        max_tokens: arguments.max_tokens,
    })?;
    let result = harness
        .run(
            arguments.prompt,
            RunOptions {
                session_id: arguments.session_id.map(SessionId::new),
                on_notification: None,
            },
        )
        .await;
    let close = harness.close().await;
    match (result, close) {
        (Ok(result), Ok(())) => Ok(result.final_response),
        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(close)) => Err(anyhow::anyhow!(
            "{error:#}; runtime cleanup failed: {close:#}"
        )),
    }
}

fn absolute(path: &std::path::Path) -> anyhow::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}
