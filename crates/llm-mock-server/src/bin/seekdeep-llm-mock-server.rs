//! Standalone JSONL-observed mock LLM server.

use std::{io::Write as _, process::ExitCode, sync::Arc};

use seekdeep_llm_mock_server::{
    cli::{MOCK_LLM_CLI_USAGE, MockLlmCliParseResult, parse_mock_llm_cli_args},
    start_mock_llm_server,
};

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "{error}\n\n{MOCK_LLM_CLI_USAGE}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> anyhow::Result<u8> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let parsed = parse_mock_llm_cli_args(&arguments)?;
    let MockLlmCliParseResult::Run(mut config) = parsed else {
        print!("{MOCK_LLM_CLI_USAGE}");
        std::io::stdout().flush()?;
        return Ok(0);
    };
    let host = config
        .server
        .host
        .clone()
        .unwrap_or_else(|| "127.0.0.1".to_owned());
    let port = config.server.port.unwrap_or(8_000.0);
    if config.starts_unavailable {
        println!(
            "{}",
            serde_json::json!({
                "type":"unavailable",
                "baseURL":format!("http://{host}:{port:.0}/v1"),
                "listenDelayMs":config.listen_delay_ms,
            })
        );
        std::io::stdout().flush()?;
        tokio::time::sleep(std::time::Duration::from_millis(config.listen_delay_ms)).await;
    }
    let output = Arc::new(parking_lot::Mutex::new(std::io::stdout()));
    let event_output = output.clone();
    config.server.on_event = Some(Arc::new(move |event| {
        if let Ok(line) = serde_json::to_string(&event) {
            let mut output = event_output.lock();
            let _ = writeln!(output, "{line}");
            let _ = output.flush();
        }
    }));
    let server = start_mock_llm_server(config.server).await?;
    {
        let mut output = output.lock();
        writeln!(
            output,
            "{}",
            serde_json::json!({
                "type":"ready",
                "baseURL":format!("{}/v1", server.base_url),
                "randomSeed":server.random_seed,
            })
        )?;
        output.flush()?;
    }
    let code = wait_for_signal().await?;
    server.close().await?;
    Ok(code)
}

#[cfg(unix)]
async fn wait_for_signal() -> anyhow::Result<u8> {
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    Ok(tokio::select! {
        _ = interrupt.recv() => 130,
        _ = terminate.recv() => 143,
    })
}

#[cfg(not(unix))]
async fn wait_for_signal() -> anyhow::Result<u8> {
    tokio::signal::ctrl_c().await?;
    Ok(130)
}
