//! Model-ordered tool-call scheduling with bounded overlap and barriers.

use std::{panic::AssertUnwindSafe, sync::Arc};

use futures::{FutureExt, StreamExt, future::BoxFuture, stream::FuturesUnordered};
use seekdeep_agent::Agent;
use seekdeep_core::session::{AppendOptions, Session, SurfaceOp};
use seekdeep_llm::{AbortSignal, CallId, ContentBlock, Message, UserMessage};
use seekdeep_scope::ScopeKey;
use seekdeep_tools::{
    ScheduledToolDispatch, ScheduledToolPreparation, TOOL_ABORTED_BEFORE_DISPATCH, ToolExecution,
    ToolExecutionFailure, ToolExecutionInput, ToolExecutionMode, ToolExecutionResult, ToolFailure,
    ToolRuntime,
};
use serde_json::{Value, json};

/// One assistant tool-call block after stream assembly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCall {
    /// Provider-issued call identity.
    pub id: CallId,
    /// Requested registered name.
    pub name: String,
    /// Raw provider-produced argument text.
    pub arguments: String,
}

impl ToolCall {
    /// Extracts a tool call from a content block.
    #[must_use]
    pub fn from_content(block: &ContentBlock) -> Option<Self> {
        match block {
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } => Some(Self {
                id: id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
            }),
            ContentBlock::Text { .. }
            | ContentBlock::Reasoning { .. }
            | ContentBlock::Image { .. }
            | ContentBlock::ToolResult { .. }
            | ContentBlock::Unknown { .. } => None,
        }
    }
}

/// Aggregate outcome of one model-ordered call batch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ToolCallBatchOutcome {
    /// Whether any committed successful result concluded the turn.
    pub concluded: bool,
}

/// Complete scheduler input for one committed assistant step.
#[derive(Clone, Copy)]
pub struct ToolCallBatch<'a> {
    /// Scope-aware staged tool runtime.
    pub runtime: &'a Arc<ToolRuntime>,
    /// Durable session receiving call/result pairs.
    pub session: &'a Arc<Session>,
    /// Exact live agent requesting the calls, when this is a live loop batch.
    pub agent: Option<&'a Arc<Agent>>,
    /// Agent scope used for tool visibility and policy routing.
    pub agent_scope: Option<ScopeKey>,
    /// Open turn number.
    pub turn: u64,
    /// Open step number.
    pub step: u64,
    /// Model-ordered calls from the committed assistant message.
    pub tool_calls: &'a [ToolCall],
    /// Owning step cancellation signal.
    pub signal: &'a AbortSignal,
    /// Maximum simultaneously dispatched parallel-safe calls.
    pub max_parallel_tool_calls: usize,
}

#[derive(Clone)]
struct PlannedCall {
    block: ToolCall,
    input: ToolExecutionInput,
}

struct Slot {
    execution: ToolExecution,
    result: ToolExecutionResult,
    needs_post: bool,
}

struct GroupOutcome {
    consumed: usize,
    aborted: bool,
    concluded: bool,
}

type Flight = BoxFuture<'static, anyhow::Result<(usize, ToolExecution, ScheduledToolDispatch)>>;

/// Schedules one assistant step's calls by their live concurrency mode.
///
/// Pre-policy, post-policy, durable results, and accepted result context remain
/// model ordered. Only the around-dispatch/body stage overlaps. Exclusive calls
/// form barriers, and every later call is reclassified immediately before it
/// starts. Cancellation drains started calls, commits them in order, then adds
/// synthetic results for calls that never reached dispatch.
///
/// # Errors
///
/// Returns session append errors or an internal staged-scheduler panic after
/// draining already-started dispatches. It never fabricates recovery results
/// for that internal-failure path.
pub async fn execute_tool_calls<F>(
    batch: ToolCallBatch<'_>,
    mut accept_context: F,
) -> anyhow::Result<ToolCallBatchOutcome>
where
    F: FnMut(UserMessage) -> anyhow::Result<()>,
{
    let ToolCallBatch {
        runtime,
        session,
        agent,
        agent_scope,
        turn,
        step,
        tool_calls,
        signal,
        max_parallel_tool_calls,
    } = batch;
    anyhow::ensure!(
        max_parallel_tool_calls > 0,
        "maxParallelToolCalls must be a positive integer"
    );
    let planned = tool_calls
        .iter()
        .map(|block| {
            let mut input = ToolExecutionInput::new(
                block.id.clone(),
                block.name.clone(),
                parse_arguments(&block.arguments),
                signal.clone(),
            );
            input = if let Some(agent) = agent {
                input.with_agent(agent.clone())
            } else {
                let input = if let Some(scope) = agent_scope {
                    input.with_agent_scope(scope)
                } else {
                    input
                };
                input.with_agent_session(session.clone())
            };
            PlannedCall {
                block: block.clone(),
                input,
            }
        })
        .collect::<Vec<_>>();

    let mut next = 0;
    let mut concluded = false;
    while next < planned.len() {
        let first = &planned[next];
        let mode = runtime.execution_mode(&first.input);
        let group = if mode == ToolExecutionMode::Parallel {
            &planned[next..]
        } else {
            &planned[next..=next]
        };
        let outcome = run_group(
            runtime,
            session,
            turn,
            step,
            group,
            mode,
            signal,
            max_parallel_tool_calls,
            &mut accept_context,
        )
        .await?;
        next += outcome.consumed;
        concluded |= outcome.concluded;
        if outcome.aborted {
            for call in &planned[next..] {
                append_skipped_tool_call(session, turn, step, &call.block)?;
            }
            return Ok(ToolCallBatchOutcome { concluded });
        }
    }
    Ok(ToolCallBatchOutcome { concluded })
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines)]
async fn run_group<F>(
    runtime: &Arc<ToolRuntime>,
    session: &Arc<Session>,
    turn: u64,
    step: u64,
    group: &[PlannedCall],
    mode: ToolExecutionMode,
    signal: &AbortSignal,
    max_parallel: usize,
    accept_context: &mut F,
) -> anyhow::Result<GroupOutcome>
where
    F: FnMut(UserMessage) -> anyhow::Result<()>,
{
    let mut slots = (0..group.len())
        .map(|_| None)
        .collect::<Vec<Option<Slot>>>();
    let mut call_seqs = vec![None; group.len()];
    let mut next_to_start = 0;
    let mut committed = 0;
    let mut started = 0;
    let mut aborted = signal.is_aborted();
    let mut concluded = false;
    let mut flights = FuturesUnordered::<Flight>::new();

    loop {
        while !aborted && next_to_start < group.len() && flights.len() < max_parallel {
            let call = &group[next_to_start];
            if next_to_start > 0
                && mode == ToolExecutionMode::Parallel
                && runtime.execution_mode(&call.input) != ToolExecutionMode::Parallel
            {
                break;
            }
            let call_seq = append_tool_call(session, turn, step, &call.block)?;
            call_seqs[next_to_start] = Some(call_seq);
            started += 1;
            let prepared = AssertUnwindSafe(runtime.prepare_scheduled(call.input.clone()))
                .catch_unwind()
                .await;
            let prepared = match prepared {
                Ok(prepared) => prepared,
                Err(panic) => {
                    drain_flights(&mut flights).await;
                    return Err(scheduler_panic(&panic));
                }
            };
            match prepared {
                ScheduledToolPreparation::Dispatch { execution } => {
                    let dispatch_runtime = runtime.clone();
                    let dispatch_execution = execution.clone();
                    let index = next_to_start;
                    flights.push(Box::pin(async move {
                        AssertUnwindSafe(dispatch_runtime.dispatch_scheduled(&dispatch_execution))
                            .catch_unwind()
                            .await
                            .map(|result| (index, dispatch_execution, result))
                            .map_err(|panic| scheduler_panic(&panic))
                    }));
                }
                ScheduledToolPreparation::PostResult { execution, result } => {
                    slots[next_to_start] = Some(Slot {
                        execution,
                        result,
                        needs_post: true,
                    });
                }
                ScheduledToolPreparation::FinalResult { execution, result } => {
                    slots[next_to_start] = Some(Slot {
                        execution,
                        result,
                        needs_post: false,
                    });
                }
            }
            next_to_start += 1;
            let commit = commit_ready(
                runtime,
                session,
                turn,
                step,
                group,
                &mut slots,
                &call_seqs,
                &mut committed,
                &mut concluded,
                accept_context,
            )
            .await;
            if let Err(error) = commit {
                drain_flights(&mut flights).await;
                return Err(error);
            }
            aborted |= signal.is_aborted();
        }

        let Some(settled) = flights.next().await else {
            break;
        };
        match settled {
            Ok((index, execution, ScheduledToolDispatch::PostResult(result))) => {
                slots[index] = Some(Slot {
                    execution,
                    result,
                    needs_post: true,
                });
            }
            Ok((index, execution, ScheduledToolDispatch::FinalResult(result))) => {
                slots[index] = Some(Slot {
                    execution,
                    result,
                    needs_post: false,
                });
            }
            Err(error) => {
                drain_flights(&mut flights).await;
                return Err(error);
            }
        }
        let commit = commit_ready(
            runtime,
            session,
            turn,
            step,
            group,
            &mut slots,
            &call_seqs,
            &mut committed,
            &mut concluded,
            accept_context,
        )
        .await;
        if let Err(error) = commit {
            drain_flights(&mut flights).await;
            return Err(error);
        }
        aborted |= signal.is_aborted();
    }

    if aborted {
        for call in &group[started..] {
            append_skipped_tool_call(session, turn, step, &call.block)?;
        }
        return Ok(GroupOutcome {
            consumed: group.len(),
            aborted: true,
            concluded,
        });
    }
    anyhow::ensure!(
        committed == started,
        "tool-call scheduler: uncommitted settled calls"
    );
    Ok(GroupOutcome {
        consumed: started,
        aborted: false,
        concluded,
    })
}

async fn drain_flights(flights: &mut FuturesUnordered<Flight>) {
    while flights.next().await.is_some() {}
}

#[allow(clippy::too_many_arguments)]
async fn commit_ready<F>(
    runtime: &Arc<ToolRuntime>,
    session: &Arc<Session>,
    turn: u64,
    step: u64,
    group: &[PlannedCall],
    slots: &mut [Option<Slot>],
    call_seqs: &[Option<u64>],
    committed: &mut usize,
    concluded: &mut bool,
    accept_context: &mut F,
) -> anyhow::Result<()>
where
    F: FnMut(UserMessage) -> anyhow::Result<()>,
{
    while let Some(slot) = slots.get_mut(*committed).and_then(Option::take) {
        let result = if slot.needs_post {
            AssertUnwindSafe(runtime.finalize_scheduled(&slot.execution, slot.result))
                .catch_unwind()
                .await
                .map_err(|panic| scheduler_panic(&panic))?
        } else {
            std::panic::catch_unwind(AssertUnwindSafe(|| {
                runtime.finish_scheduled(&slot.execution, slot.result)
            }))
            .map_err(|panic| scheduler_panic(&panic))?
        };
        let call = &group[*committed].block;
        let call_seq = call_seqs[*committed]
            .ok_or_else(|| anyhow::anyhow!("tool-call scheduler: missing call sequence"))?;
        append_tool_result(session, turn, step, call, &result, call_seq)?;
        for context in result.additional_contexts() {
            accept_context(context.clone())?;
        }
        *concluded |= result.concludes_turn();
        *committed += 1;
    }
    Ok(())
}

fn parse_arguments(raw: &str) -> Value {
    if raw.is_empty() {
        return json!({});
    }
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
}

fn append_tool_call(
    session: &Session,
    turn: u64,
    step: u64,
    block: &ToolCall,
) -> anyhow::Result<u64> {
    Ok(session
        .append(
            "tool/call",
            json!({
                "turn": turn,
                "step": step,
                "callId": block.id,
                "name": block.name,
                "arguments": block.arguments,
            }),
            AppendOptions::default(),
        )?
        .seq)
}

fn append_skipped_tool_call(
    session: &Session,
    turn: u64,
    step: u64,
    block: &ToolCall,
) -> anyhow::Result<()> {
    let call_seq = append_tool_call(session, turn, step, block)?;
    let result = ToolExecutionResult::Failure(ToolExecutionFailure {
        content: vec![ContentBlock::Text {
            text: "Error: tool call aborted before dispatch".to_owned(),
        }],
        error: ToolFailure {
            message: "tool call aborted before dispatch".to_owned(),
            info: Some(seekdeep_tools::ToolErrorInfo {
                name: "AbortError".to_owned(),
                code: TOOL_ABORTED_BEFORE_DISPATCH.to_owned(),
            }),
        },
        meta: None,
        additional_contexts: Vec::new(),
    });
    append_tool_result(session, turn, step, block, &result, call_seq)
}

fn append_tool_result(
    session: &Session,
    turn: u64,
    step: u64,
    block: &ToolCall,
    result: &ToolExecutionResult,
    call_seq: u64,
) -> anyhow::Result<()> {
    let message = Message::tool_result(&block.id, result.content().to_vec(), result.is_error());
    let mut data = serde_json::Map::new();
    data.insert("turn".to_owned(), Value::from(turn));
    data.insert("step".to_owned(), Value::from(step));
    data.insert("message".to_owned(), serde_json::to_value(message)?);
    if let Some(info) = result.error().and_then(|error| error.info.as_ref()) {
        data.insert(
            "error".to_owned(),
            json!({"name": info.name, "code": info.code}),
        );
    }
    if let Some(meta) = result.meta() {
        data.insert("meta".to_owned(), meta.clone());
    }
    session.append(
        "tool/result",
        Value::Object(data),
        AppendOptions {
            surface_op: Some(SurfaceOp::append()),
            source_event_seqs: Some(vec![call_seq]),
            ..AppendOptions::default()
        },
    )?;
    Ok(())
}

fn scheduler_panic(panic: &Box<dyn std::any::Any + Send>) -> anyhow::Error {
    let message = panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("tool scheduler panicked");
    anyhow::anyhow!(message.to_owned())
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Arc, time::Duration};

    use parking_lot::Mutex;
    use seekdeep_core::session::SessionId;
    use seekdeep_tools::{
        ToolDefinition, ToolOutputDefinition, ToolRuntimeConfig, assert_supported_json_schema,
    };
    use serde_json::Map;
    use tokio::sync::{Notify, oneshot};

    use super::*;

    fn definition(
        name: &str,
        execute: seekdeep_tools::runtime::ToolExecute,
        parallel: bool,
    ) -> ToolDefinition {
        let definition = ToolDefinition::new(
            name,
            format!("{name} description"),
            Map::from_iter([("type".to_owned(), Value::String("object".to_owned()))]),
            ToolOutputDefinition::new(
                Arc::new(
                    assert_supported_json_schema(json!({"type": "string"})).expect("output schema"),
                ),
                Arc::new(|_, value| {
                    Ok(vec![ContentBlock::Text {
                        text: value.as_str().unwrap_or_default().to_owned(),
                    }])
                }),
            ),
            execute,
        );
        if parallel {
            definition.concurrency_safe(Arc::new(|_| true))
        } else {
            definition
        }
    }

    fn call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: CallId::new(id),
            name: name.to_owned(),
            arguments: String::new(),
        }
    }

    fn call_with_argument(id: &str, name: &str, argument: &str) -> ToolCall {
        ToolCall {
            id: CallId::new(id),
            name: name.to_owned(),
            arguments: json!({"id": argument}).to_string(),
        }
    }

    fn result_call_ids(session: &Session) -> Vec<String> {
        session
            .events()
            .into_iter()
            .filter(|event| event.event_type == "tool/result")
            .filter_map(|event| {
                event
                    .data
                    .get("message")?
                    .get("source")?
                    .get("callId")?
                    .as_str()
                    .map(str::to_owned)
            })
            .collect()
    }

    #[tokio::test]
    async fn overlaps_parallel_bodies_but_commits_results_in_model_order() {
        let context = seekdeep_cordis::Context::new();
        let runtime =
            ToolRuntime::new(context.clone(), ToolRuntimeConfig::default()).expect("runtime");
        let (release_a, wait_a) = oneshot::channel::<()>();
        let wait_a = Arc::new(Mutex::new(Some(wait_a)));
        runtime
            .register(
                &context,
                definition(
                    "a",
                    Arc::new(move |_, _| {
                        let wait = wait_a.lock().take().expect("a called once");
                        Box::pin(async move {
                            let _ = wait.await;
                            Ok(Value::String("a-result".to_owned()))
                        })
                    }),
                    true,
                ),
            )
            .expect("a");
        let b_started = Arc::new(Notify::new());
        runtime
            .register(
                &context,
                definition(
                    "b",
                    Arc::new({
                        let b_started = b_started.clone();
                        move |_, _| {
                            b_started.notify_one();
                            Box::pin(async { Ok(Value::String("b-result".to_owned())) })
                        }
                    }),
                    true,
                ),
            )
            .expect("b");
        let session = Session::create(&SessionId::new("parallel"), None, None).expect("session");
        let calls = vec![call("call-a", "a"), call("call-b", "b")];
        let signal = AbortSignal::default();
        let task_runtime = runtime.clone();
        let task_session = session.clone();
        let task = tokio::spawn(async move {
            execute_tool_calls(
                ToolCallBatch {
                    runtime: &task_runtime,
                    session: &task_session,
                    agent: None,
                    agent_scope: None,
                    turn: 1,
                    step: 1,
                    tool_calls: &calls,
                    signal: &signal,
                    max_parallel_tool_calls: 2,
                },
                |_| Ok(()),
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), b_started.notified())
            .await
            .expect("b overlaps blocked a");
        assert!(!task.is_finished());
        release_a.send(()).expect("release a");
        task.await.expect("join").expect("execute");
        assert_eq!(result_call_ids(&session), ["call-a", "call-b"]);
    }

    #[tokio::test]
    async fn preaborted_batch_records_synthetic_pairs_without_dispatch() {
        let context = seekdeep_cordis::Context::new();
        let runtime =
            ToolRuntime::new(context.clone(), ToolRuntimeConfig::default()).expect("runtime");
        let invoked = Arc::new(std::sync::atomic::AtomicBool::new(false));
        runtime
            .register(
                &context,
                definition(
                    "never",
                    Arc::new({
                        let invoked = invoked.clone();
                        move |_, _| {
                            invoked.store(true, std::sync::atomic::Ordering::Release);
                            Box::pin(async { Ok(Value::String("unexpected".to_owned())) })
                        }
                    }),
                    false,
                ),
            )
            .expect("register");
        let session = Session::create(&SessionId::new("aborted"), None, None).expect("session");
        let signal = AbortSignal::default();
        signal.abort();
        execute_tool_calls(
            ToolCallBatch {
                runtime: &runtime,
                session: &session,
                agent: None,
                agent_scope: None,
                turn: 1,
                step: 1,
                tool_calls: &[call("one", "never"), call("two", "never")],
                signal: &signal,
                max_parallel_tool_calls: 2,
            },
            |_| Ok(()),
        )
        .await
        .expect("execute");
        assert!(!invoked.load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(result_call_ids(&session), ["one", "two"]);
        for result in session
            .events()
            .into_iter()
            .filter(|event| event.event_type == "tool/result")
        {
            assert_eq!(result.data["error"]["code"], TOOL_ABORTED_BEFORE_DISPATCH);
        }
    }

    #[tokio::test]
    async fn exclusive_calls_are_barriers_between_parallel_groups() {
        let context = seekdeep_cordis::Context::new();
        let runtime =
            ToolRuntime::new(context.clone(), ToolRuntimeConfig::default()).expect("runtime");
        let order = Arc::new(Mutex::new(Vec::<String>::new()));
        for (name, parallel) in [("read-a", true), ("write", false), ("read-b", true)] {
            runtime
                .register(
                    &context,
                    definition(
                        name,
                        Arc::new({
                            let order = order.clone();
                            let name = name.to_owned();
                            move |_, _| {
                                order.lock().push(name.clone());
                                Box::pin(async { Ok(Value::String("ok".to_owned())) })
                            }
                        }),
                        parallel,
                    ),
                )
                .expect("register");
        }
        let session = Session::create(&SessionId::new("barriers"), None, None).expect("session");
        execute_tool_calls(
            ToolCallBatch {
                runtime: &runtime,
                session: &session,
                agent: None,
                agent_scope: None,
                turn: 1,
                step: 1,
                tool_calls: &[
                    call("one", "read-a"),
                    call("two", "write"),
                    call("three", "read-b"),
                ],
                signal: &AbortSignal::default(),
                max_parallel_tool_calls: 3,
            },
            |_| Ok(()),
        )
        .await
        .expect("execute");
        assert_eq!(
            &*order.lock(),
            &vec!["read-a".to_owned(), "write".to_owned(), "read-b".to_owned()]
        );
        assert_eq!(result_call_ids(&session), ["one", "two", "three"]);
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn pending_calls_are_reclassified_after_an_exclusive_barrier() {
        let context = seekdeep_cordis::Context::new();
        let runtime =
            ToolRuntime::new(context.clone(), ToolRuntimeConfig::default()).expect("runtime");
        let original = runtime
            .register(
                &context,
                definition(
                    "x",
                    Arc::new(|_, _| Box::pin(async { Ok(Value::String("obsolete".to_owned())) })),
                    true,
                ),
            )
            .expect("original x");
        let releases = Arc::new(Mutex::new(HashMap::<String, oneshot::Sender<()>>::new()));
        let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let replacement = definition(
            "x",
            Arc::new({
                let releases = releases.clone();
                move |arguments, _| {
                    let id = arguments["id"].as_str().expect("id").to_owned();
                    let (release, wait) = oneshot::channel();
                    releases.lock().insert(id.clone(), release);
                    started_tx.send(id).expect("start observer");
                    Box::pin(async move {
                        let _ = wait.await;
                        Ok(Value::String("replacement".to_owned()))
                    })
                }
            }),
            false,
        );
        runtime
            .register(
                &context,
                definition(
                    "replace",
                    Arc::new({
                        let runtime = runtime.clone();
                        let context = context.clone();
                        move |_, _| {
                            let original = original.clone();
                            let runtime = runtime.clone();
                            let context = context.clone();
                            let replacement = replacement.clone();
                            Box::pin(async move {
                                original.dispose().await?;
                                runtime.register(&context, replacement)?;
                                Ok(Value::String("replaced".to_owned()))
                            })
                        }
                    }),
                    false,
                ),
            )
            .expect("replace");
        let session = Session::create(&SessionId::new("reclassify"), None, None).expect("session");
        let task_runtime = runtime.clone();
        let task_session = session.clone();
        let task = tokio::spawn(async move {
            execute_tool_calls(
                ToolCallBatch {
                    runtime: &task_runtime,
                    session: &task_session,
                    agent: None,
                    agent_scope: None,
                    turn: 1,
                    step: 1,
                    tool_calls: &[
                        call("replace", "replace"),
                        call_with_argument("x1", "x", "1"),
                        call_with_argument("x2", "x", "2"),
                    ],
                    signal: &AbortSignal::default(),
                    max_parallel_tool_calls: 3,
                },
                |_| Ok(()),
            )
            .await
        });
        assert_eq!(started_rx.recv().await.as_deref(), Some("1"));
        assert!(
            tokio::time::timeout(Duration::from_millis(20), started_rx.recv())
                .await
                .is_err(),
            "the newly exclusive sibling overlapped"
        );
        releases
            .lock()
            .remove("1")
            .expect("release one")
            .send(())
            .ok();
        assert_eq!(started_rx.recv().await.as_deref(), Some("2"));
        releases
            .lock()
            .remove("2")
            .expect("release two")
            .send(())
            .ok();
        task.await.expect("join").expect("execute");
        assert_eq!(result_call_ids(&session), ["replace", "x1", "x2"]);
    }

    #[tokio::test]
    async fn bounded_pool_replenishes_without_exceeding_its_cap() {
        let context = seekdeep_cordis::Context::new();
        let runtime =
            ToolRuntime::new(context.clone(), ToolRuntimeConfig::default()).expect("runtime");
        let releases = Arc::new(Mutex::new(HashMap::<String, oneshot::Sender<()>>::new()));
        let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        runtime
            .register(
                &context,
                definition(
                    "gated",
                    Arc::new({
                        let releases = releases.clone();
                        move |arguments, _| {
                            let id = arguments["id"].as_str().expect("id").to_owned();
                            let (release, wait) = oneshot::channel();
                            releases.lock().insert(id.clone(), release);
                            started_tx.send(id).expect("start observer");
                            Box::pin(async move {
                                let _ = wait.await;
                                Ok(Value::String("ok".to_owned()))
                            })
                        }
                    }),
                    true,
                ),
            )
            .expect("register");
        let session = Session::create(&SessionId::new("bounded"), None, None).expect("session");
        let task_runtime = runtime.clone();
        let task_session = session.clone();
        let task = tokio::spawn(async move {
            execute_tool_calls(
                ToolCallBatch {
                    runtime: &task_runtime,
                    session: &task_session,
                    agent: None,
                    agent_scope: None,
                    turn: 1,
                    step: 1,
                    tool_calls: &[
                        call_with_argument("c1", "gated", "1"),
                        call_with_argument("c2", "gated", "2"),
                        call_with_argument("c3", "gated", "3"),
                        call_with_argument("c4", "gated", "4"),
                    ],
                    signal: &AbortSignal::default(),
                    max_parallel_tool_calls: 2,
                },
                |_| Ok(()),
            )
            .await
        });
        let mut first = vec![
            started_rx.recv().await.expect("first"),
            started_rx.recv().await.expect("second"),
        ];
        first.sort();
        assert_eq!(first, ["1", "2"]);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), started_rx.recv())
                .await
                .is_err(),
            "a third call started above the cap"
        );
        releases
            .lock()
            .remove("1")
            .expect("release 1")
            .send(())
            .ok();
        assert_eq!(started_rx.recv().await.as_deref(), Some("3"));
        for id in ["2", "3"] {
            releases.lock().remove(id).expect("release").send(()).ok();
        }
        assert_eq!(started_rx.recv().await.as_deref(), Some("4"));
        releases
            .lock()
            .remove("4")
            .expect("release 4")
            .send(())
            .ok();
        task.await.expect("join").expect("execute");
        assert_eq!(result_call_ids(&session), ["c1", "c2", "c3", "c4"]);
    }

    #[tokio::test]
    async fn abort_drains_started_calls_and_synthesizes_every_unstarted_pair() {
        let context = seekdeep_cordis::Context::new();
        let runtime =
            ToolRuntime::new(context.clone(), ToolRuntimeConfig::default()).expect("runtime");
        let releases = Arc::new(Mutex::new(HashMap::<String, oneshot::Sender<()>>::new()));
        let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        runtime
            .register(
                &context,
                definition(
                    "gated",
                    Arc::new({
                        let releases = releases.clone();
                        move |arguments, _| {
                            let id = arguments["id"].as_str().expect("id").to_owned();
                            let (release, wait) = oneshot::channel();
                            releases.lock().insert(id.clone(), release);
                            started_tx.send(id).expect("start observer");
                            Box::pin(async move {
                                let _ = wait.await;
                                Ok(Value::String("ok".to_owned()))
                            })
                        }
                    }),
                    true,
                ),
            )
            .expect("register");
        let session = Session::create(&SessionId::new("abort-drain"), None, None).expect("session");
        let signal = AbortSignal::default();
        let task_runtime = runtime.clone();
        let task_session = session.clone();
        let task_signal = signal.clone();
        let task = tokio::spawn(async move {
            execute_tool_calls(
                ToolCallBatch {
                    runtime: &task_runtime,
                    session: &task_session,
                    agent: None,
                    agent_scope: None,
                    turn: 1,
                    step: 1,
                    tool_calls: &[
                        call_with_argument("c1", "gated", "1"),
                        call_with_argument("c2", "gated", "2"),
                        call_with_argument("c3", "gated", "3"),
                        call_with_argument("c4", "gated", "4"),
                    ],
                    signal: &task_signal,
                    max_parallel_tool_calls: 2,
                },
                |_| Ok(()),
            )
            .await
        });
        let _ = started_rx.recv().await.expect("first");
        let _ = started_rx.recv().await.expect("second");
        signal.abort();
        for id in ["1", "2"] {
            releases.lock().remove(id).expect("release").send(()).ok();
        }
        task.await.expect("join").expect("execute");
        assert!(started_rx.try_recv().is_err());
        assert_eq!(result_call_ids(&session), ["c1", "c2", "c3", "c4"]);
        let events = session.events();
        let synthetic_codes = events
            .iter()
            .filter(|event| event.event_type == "tool/result")
            .skip(2)
            .map(|event| event.data["error"]["code"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            synthetic_codes,
            [
                Some(TOOL_ABORTED_BEFORE_DISPATCH),
                Some(TOOL_ABORTED_BEFORE_DISPATCH)
            ]
        );
    }

    #[tokio::test]
    async fn accepted_contexts_follow_model_order_not_settlement_order() {
        let context = seekdeep_cordis::Context::new();
        let runtime =
            ToolRuntime::new(context.clone(), ToolRuntimeConfig::default()).expect("runtime");
        let (release_first, wait_first) = oneshot::channel::<()>();
        let wait_first = Arc::new(Mutex::new(Some(wait_first)));
        runtime
            .register(
                &context,
                definition(
                    "first",
                    Arc::new(move |_, _| {
                        let wait = wait_first.lock().take().expect("first once");
                        Box::pin(async move {
                            let _ = wait.await;
                            Ok(Value::String("first".to_owned()))
                        })
                    }),
                    true,
                ),
            )
            .expect("first");
        let second_started = Arc::new(Notify::new());
        runtime
            .register(
                &context,
                definition(
                    "second",
                    Arc::new({
                        let second_started = second_started.clone();
                        move |_, _| {
                            second_started.notify_one();
                            Box::pin(async { Ok(Value::String("second".to_owned())) })
                        }
                    }),
                    true,
                ),
            )
            .expect("second");
        runtime
            .on_post_execute(
                &context,
                |execution, _result, _next| async move {
                    Ok(seekdeep_tools::PostToolDecision::Accept {
                        content: None,
                        additional_contexts: vec![UserMessage::new(
                            vec![ContentBlock::Text {
                                text: execution.call_id.as_str().to_owned(),
                            }],
                            seekdeep_llm::MessageSource::plugin("test"),
                        )],
                    })
                },
                seekdeep_cordis::EventOptions::default(),
            )
            .expect("post");
        let session = Session::create(&SessionId::new("contexts"), None, None).expect("session");
        let accepted = Arc::new(Mutex::new(Vec::<String>::new()));
        let task_runtime = runtime.clone();
        let task_session = session.clone();
        let task_accepted = accepted.clone();
        let task = tokio::spawn(async move {
            execute_tool_calls(
                ToolCallBatch {
                    runtime: &task_runtime,
                    session: &task_session,
                    agent: None,
                    agent_scope: None,
                    turn: 1,
                    step: 1,
                    tool_calls: &[call("c1", "first"), call("c2", "second")],
                    signal: &AbortSignal::default(),
                    max_parallel_tool_calls: 2,
                },
                move |message| {
                    let ContentBlock::Text { text } = &message.content()[0] else {
                        anyhow::bail!("unexpected context")
                    };
                    task_accepted.lock().push(text.clone());
                    Ok(())
                },
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(1), second_started.notified())
            .await
            .expect("second settled");
        assert!(accepted.lock().is_empty());
        release_first.send(()).expect("release first");
        task.await.expect("join").expect("execute");
        assert_eq!(&*accepted.lock(), &vec!["c1".to_owned(), "c2".to_owned()]);
    }

    #[tokio::test]
    async fn zero_parallelism_is_rejected_before_any_durable_call() {
        let context = seekdeep_cordis::Context::new();
        let runtime =
            ToolRuntime::new(context.clone(), ToolRuntimeConfig::default()).expect("runtime");
        let session = Session::create(&SessionId::new("invalid-cap"), None, None).expect("session");
        let error = execute_tool_calls(
            ToolCallBatch {
                runtime: &runtime,
                session: &session,
                agent: None,
                agent_scope: None,
                turn: 1,
                step: 1,
                tool_calls: &[call("c1", "missing")],
                signal: &AbortSignal::default(),
                max_parallel_tool_calls: 0,
            },
            |_| Ok(()),
        )
        .await
        .expect_err("invalid cap");
        assert!(error.to_string().contains("positive integer"));
        assert!(session.events().is_empty());
    }
}
