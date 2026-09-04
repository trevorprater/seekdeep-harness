//! Source-derived old-generation limits through the public runtime service.

use seekdeep_code_runtime::{CodeRunFailureKind, CodeRunRequest, CodeRuntimeBackend};
use seekdeep_code_runtime_worker_thread::{WorkerThreadCodeRuntime, WorkerThreadCodeRuntimeConfig};
use serde_json::json;

const BOUNDED_ALLOCATION: &str = "console.log('before'); const retained = []; for (let i = 0; i < 8; i++) retained.push(new Array(1000000).fill(i)); return retained.length;";

fn request(program: &str) -> CodeRunRequest {
    CodeRunRequest {
        program: program.to_owned(),
        bindings: Vec::new(),
        signal: None,
    }
}

fn runtime(heap_mb: f64) -> WorkerThreadCodeRuntime {
    WorkerThreadCodeRuntime::new(&WorkerThreadCodeRuntimeConfig {
        compute_ms: Some(5_000.0),
        max_wall_ms: Some(10_000.0),
        max_output_bytes: Some(1_024.0),
        max_old_generation_size_mb: Some(heap_mb),
    })
    .unwrap()
}

#[tokio::test]
async fn heap_limit_stops_only_the_over_budget_worker_and_preserves_logs() {
    // Finite allocation also fails safely on a backend that ignores the cap.
    let limited = runtime(16.0);
    let result = limited.run(request(BOUNDED_ALLOCATION)).await.unwrap();
    assert_eq!(result.logs, ["before"]);
    let failure = result.error.expect("16 MiB must reject the retained heap");
    assert_eq!(failure.kind, CodeRunFailureKind::WorkerExit);
    assert_eq!(
        failure.message,
        "worker error: Worker terminated due to reaching memory limit: JS heap out of memory"
    );
    assert_eq!(
        limited.run(request("return 'alive'")).await.unwrap().value,
        Some(json!("alive"))
    );

    let roomy = runtime(512.0)
        .run(request(BOUNDED_ALLOCATION))
        .await
        .unwrap();
    assert_eq!(roomy.logs, ["before"]);
    assert_eq!(roomy.error, None);
    assert_eq!(roomy.value, Some(json!(8)));
}
