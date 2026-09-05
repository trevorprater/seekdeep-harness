//! Standalone Rust Cordis composition launcher.

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let result = async {
        let cwd = std::env::current_dir()?;
        seekdeep_loader::launcher::run_cordis_file(&cwd, async {
            Ok(seekdeep_loader::launcher::termination_signal().await?)
        })
        .await
    }
    .await;
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("cordis: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
