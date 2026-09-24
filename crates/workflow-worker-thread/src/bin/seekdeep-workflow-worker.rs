//! Stdio entry for one killable workflow evaluator process.

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = seekdeep_workflow_worker_thread::worker::run_stdio_worker().await {
        eprintln!("seekdeep-workflow-worker: {error:#}");
        std::process::exit(1);
    }
}
