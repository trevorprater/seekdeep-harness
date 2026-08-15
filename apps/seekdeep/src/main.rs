//! `SeekDeep` Harness command-line entry point.

use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "seekdeep", version, about = "SeekDeep Harness")]
struct Args {
    /// Named composition profile.
    #[arg(long, default_value = "web")]
    profile: String,
    /// Print the fully layered plugin configuration and exit.
    #[arg(long)]
    dump_config: bool,
    /// One-shot task for the headless profile.
    task: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
    if args.dump_config {
        println!("profile: {}", args.profile);
        return Ok(());
    }
    if let Some(task) = args.task {
        tracing::info!(profile = %args.profile, %task, "starting headless task");
    }
    Ok(())
}
