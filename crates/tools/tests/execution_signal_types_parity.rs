//! Compile-time source-parity checks for execution signal view capabilities.

use std::sync::Arc;

use seekdeep_cordis::{Context, EventOptions};
use seekdeep_llm::AbortSignal;
use seekdeep_tools::{
    ToolDispatchExecution, ToolExecute, ToolExecution, ToolExecutionInput, ToolRunContext,
    ToolRuntime, ToolRuntimeConfig,
};

fn input_signal(input: &ToolExecutionInput) -> &AbortSignal {
    input.signal()
}

fn execution_signal(execution: &ToolExecution) -> AbortSignal {
    execution.signal()
}

fn run_signal(run: &ToolRunContext) -> AbortSignal {
    run.signal()
}

fn dispatch_signal(dispatch: &ToolDispatchExecution) -> AbortSignal {
    dispatch.signal()
}

fn accepts_typed_body(_: ToolExecute) {}

#[test]
fn requires_exact_abort_signal_and_separates_mutation_capabilities() {
    let _: fn(&ToolExecutionInput) -> &AbortSignal = input_signal;
    let _: fn(&ToolExecution) -> AbortSignal = execution_signal;
    let _: fn(&ToolRunContext) -> AbortSignal = run_signal;
    let _: fn(&ToolDispatchExecution) -> AbortSignal = dispatch_signal;

    accepts_typed_body(Arc::new(|_, run: ToolRunContext| {
        let _: AbortSignal = run.signal();
        Box::pin(async { Ok(serde_json::Value::Null) })
    }));

    let context = Context::new();
    let runtime = ToolRuntime::new(context.clone(), ToolRuntimeConfig::default()).expect("runtime");
    runtime
        .on_execute(
            &context,
            |dispatch: ToolDispatchExecution, next| async move {
                let prior: AbortSignal = dispatch.replace_dispatch_signal(AbortSignal::default());
                let result = next.run().await;
                let _derived: AbortSignal = dispatch.replace_dispatch_signal(prior);
                result
            },
            EventOptions::default(),
        )
        .expect("around-dispatch middleware");
}
