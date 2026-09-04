//! Source-derived evaluator behavior independent of the JavaScript engine.

use super::*;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use indexmap::IndexMap;
    use seekdeep_code_runtime::{CodeBindingErrorClass, CodeBindingFunction, CodeBindingNamespace};
    use serde_json::json;

    use super::*;

    fn binding(
        function: impl Fn(Value) -> anyhow::Result<Value> + Send + Sync + 'static,
    ) -> CodeBindingFunction {
        Arc::new(move |argument| {
            let result = function(argument);
            Box::pin(async move { result })
        })
    }

    fn tools(functions: IndexMap<String, CodeBindingFunction>) -> Vec<CodeBindingNamespace> {
        vec![CodeBindingNamespace {
            global: "tools".to_owned(),
            functions,
            error_class: Some(CodeBindingErrorClass {
                name: "ToolCallError".to_owned(),
                member_name_property: "toolName".to_owned(),
            }),
        }]
    }

    fn limits(max_output_bytes: usize) -> EngineLimits {
        EngineLimits {
            max_output_bytes,
            max_old_generation_size_mb: 512.0,
            compute_ms: 60_000.0,
            max_wall_ms: 2_000.0,
            signal: AbortSignal::default(),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn evaluates_async_typescript_captures_node_like_globals_and_output() {
        let outcome = evaluate_program(
            "interface Point { x: number; y: number }; const p: Point = { x: 1, y: 2 } as Point; console.log('point', p); process.stdout.write('raw-out\\n'); console.warn('careful'); return await Promise.resolve(p.x + p.y);",
            limits(1_000),
            Vec::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            outcome.logs,
            vec!["point { x: 1, y: 2 }", "raw-out\n", "careful"]
        );
        assert!(
            matches!(outcome.completion, EngineCompletion::Success(Some(value)) if value == json!(3))
        );

        let environment = evaluate_program(
            "return JSON.stringify(process.env)",
            limits(1_000),
            Vec::new(),
        )
        .await
        .unwrap();
        assert!(
            matches!(environment.completion, EngineCompletion::Success(Some(value)) if value == json!("{}"))
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn classifies_exception_invalid_absent_pending_exit_and_limit() {
        let thrown = evaluate_program(
            "console.log('before'); throw new Error('boom')",
            limits(1_000),
            Vec::new(),
        )
        .await
        .unwrap();
        assert_eq!(thrown.logs, ["before"]);
        assert!(
            matches!(thrown.completion, EngineCompletion::Exception(message) if message.contains("boom"))
        );

        assert!(matches!(
            evaluate_program("return { f: () => 1 }", limits(1_000), Vec::new())
                .await
                .unwrap()
                .completion,
            EngineCompletion::InvalidOutput
        ));
        assert!(matches!(
            evaluate_program("const x = 1", limits(1_000), Vec::new())
                .await
                .unwrap()
                .completion,
            EngineCompletion::Success(None)
        ));
        assert!(matches!(
            evaluate_program(
                "return await new Promise(() => {})",
                EngineLimits {
                    max_wall_ms: 20.0,
                    ..limits(1_000)
                },
                Vec::new(),
            )
            .await
            .unwrap()
            .completion,
            EngineCompletion::WallTimeout
        ));
        assert!(matches!(
            evaluate_program("process.exit(7)", limits(1_000), Vec::new())
                .await
                .unwrap()
                .completion,
            EngineCompletion::WorkerExit(7)
        ));
        let limited = evaluate_program(
            "console.log('x'.repeat(1000)); return 1",
            limits(64),
            Vec::new(),
        )
        .await
        .unwrap();
        assert!(matches!(limited.completion, EngineCompletion::OutputLimit));
        assert_eq!(limited.logs.len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn captures_symbols_bigints_and_negative_zero_without_coercion_errors() {
        let outcome = evaluate_program(
            "console.log(Symbol('x'), Symbol(), 42n, -0); return 42",
            limits(1_000),
            Vec::new(),
        )
        .await
        .unwrap();
        assert_eq!(outcome.logs, ["Symbol(x) Symbol() 42n -0"]);
        assert!(
            matches!(outcome.completion, EngineCompletion::Success(Some(value)) if value == json!(42))
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn preserves_source_json_quoting_in_unknown_binding_diagnostics() {
        let outcome = evaluate_program(
            r"const { parentPort } = await import('node:worker_threads'); return await new Promise(resolve => { parentPort.on('message', reply => resolve(reply.message)); parentPort.postMessage({ type: 'call', id: 11, global: 'tools', name: '\u0007', args: [null] }); });",
            limits(1_000),
            Vec::new(),
        )
        .await
        .unwrap();
        assert!(
            matches!(&outcome.completion, EngineCompletion::Success(Some(value)) if value == &json!("unknown binding \"tools.\\u0007\"")),
            "{:?}",
            outcome.completion
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn preserves_source_catchable_exit_and_timer_coercion_failures() {
        let outcome = evaluate_program(
            "let exit = false; let timer = false; try { process.exit(Symbol()) } catch { exit = true } try { setTimeout(() => {}, Symbol()) } catch { timer = true } return { exit, timer };",
            limits(1_000),
            Vec::new(),
        )
        .await
        .unwrap();
        assert!(
            matches!(&outcome.completion, EngineCompletion::Success(Some(value)) if value == &json!({ "exit": true, "timer": true })),
            "{:?}",
            outcome.completion
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stdout_callback_and_timer_are_asynchronous_and_driven() {
        let outcome = evaluate_program(
            "await new Promise(resolve => process.stdout.write('flushed', resolve)); await new Promise(resolve => setTimeout(resolve, 5)); return 'done'",
            limits(1_000),
            Vec::new(),
        )
        .await
        .unwrap();
        assert_eq!(outcome.logs, ["flushed"]);
        assert!(
            matches!(outcome.completion, EngineCompletion::Success(Some(value)) if value == json!("done"))
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn bridges_calls_resolutions_and_typed_rejections() {
        let bindings = tools(IndexMap::from([
            (
                "echo".to_owned(),
                binding(|argument| Ok(json!({ "echoed": argument }))),
            ),
            ("fail".to_owned(), binding(|_| anyhow::bail!("nope"))),
        ]));
        let outcome = evaluate_program(
            "const first = await tools.echo({ n: 1 }); let caught; try { await tools.fail({}) } catch (error) { caught = { typed: error instanceof ToolCallError, name: error.name, toolName: error.toolName, message: error.message }; } return { first, caught };",
            limits(10_000),
            bindings,
        )
        .await
        .unwrap();
        assert!(
            matches!(outcome.completion, EngineCompletion::Success(Some(value)) if value == json!({
                "first": { "echoed": { "n": 1 } },
                "caught": { "typed": true, "name": "ToolCallError", "toolName": "fail", "message": "nope" }
            }))
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn bridges_deep_json_and_prototype_colliding_member_names() {
        let mut functions = IndexMap::new();
        functions.insert("echo".to_owned(), binding(Ok));
        functions.insert("__proto__".to_owned(), binding(|_| Ok(json!("proto-ok"))));
        functions.insert("constructor".to_owned(), binding(|_| Ok(json!("ctor-ok"))));
        let outcome = evaluate_program(
            "let value = 'leaf'; for (let depth = 0; depth < 3000; depth++) value = [value]; const echoed = await tools.echo(value); return { echoed, collisions: [await tools['__proto__']({}), await tools['constructor']({}), typeof tools['hasOwnProperty']] };",
            limits(10_000_000),
            tools(functions),
        )
        .await
        .unwrap();
        let EngineCompletion::Success(Some(mut value)) = outcome.completion else {
            panic!("deep binding did not complete")
        };
        assert_eq!(
            value["collisions"],
            json!(["proto-ok", "ctor-ok", "undefined"])
        );
        let echoed = value.as_object_mut().unwrap().remove("echoed").unwrap();
        let mut cursor = &echoed;
        for _ in 0..3_000 {
            cursor = cursor.as_array().unwrap().first().unwrap();
        }
        assert_eq!(cursor, "leaf");
        std::mem::forget(echoed);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_lossy_arguments_before_calling_host() {
        let called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = called.clone();
        let bindings = tools(IndexMap::from([(
            "never".to_owned(),
            binding(move |_| {
                observed.store(true, std::sync::atomic::Ordering::Release);
                Ok(Value::Null)
            }),
        )]));
        let outcome = evaluate_program(
            "const decorated = [1]; Object.defineProperty(decorated, 'extra', { value: true }); try { await tools.never(decorated) } catch (error) { return { typed: error instanceof ToolCallError, name: error.name, toolName: error.toolName, message: error.message }; }",
            limits(10_000),
            bindings,
        )
        .await
        .unwrap();
        assert!(!called.load(std::sync::atomic::Ordering::Acquire));
        assert!(
            matches!(outcome.completion, EngineCompletion::Success(Some(value)) if value == json!({
                "typed": true,
                "name": "ToolCallError",
                "toolName": "never",
                "message": "binding arguments must be lossless JSON"
            }))
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn compute_budget_interrupts_hot_loop_but_excludes_binding_wait() {
        let hot = evaluate_program(
            "for (;;) {}",
            EngineLimits {
                compute_ms: 25.0,
                max_wall_ms: 2_000.0,
                ..limits(1_000)
            },
            Vec::new(),
        )
        .await
        .unwrap();
        assert!(matches!(hot.completion, EngineCompletion::ComputeTimeout));
        let resumed_hot = evaluate_program(
            "await Promise.resolve(); for (;;) {}",
            EngineLimits {
                compute_ms: 5.0,
                max_wall_ms: 2_000.0,
                ..limits(1_000)
            },
            Vec::new(),
        )
        .await
        .unwrap();
        assert!(matches!(
            resumed_hot.completion,
            EngineCompletion::ComputeTimeout
        ));

        let slow: CodeBindingFunction = Arc::new(|_| {
            Box::pin(async {
                tokio::time::sleep(Duration::from_millis(250)).await;
                Ok(json!("slow-done"))
            })
        });
        let waited = evaluate_program(
            "return await tools.slow({})",
            EngineLimits {
                compute_ms: 100.0,
                max_wall_ms: 2_000.0,
                ..limits(1_000)
            },
            tools(IndexMap::from([("slow".to_owned(), slow)])),
        )
        .await
        .unwrap();
        assert!(
            matches!(&waited.completion, EngineCompletion::Success(Some(value)) if value == &json!("slow-done")),
            "unexpected completion: {:?}",
            waited.completion
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_interrupts_hot_loop_with_exact_reason() {
        let signal = AbortSignal::default();
        let cancelling = signal.clone();
        let cancellation = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            cancelling.abort_with_reason(json!("user-cancel"));
        });
        let cancelled = evaluate_program(
            "for (;;) {}",
            EngineLimits {
                compute_ms: 2_000.0,
                max_wall_ms: 2_000.0,
                signal,
                ..limits(1_000)
            },
            Vec::new(),
        )
        .await
        .unwrap();
        assert!(
            matches!(cancelled.completion, EngineCompletion::Abort(reason) if reason == json!("user-cancel"))
        );
        cancellation.join().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn boundary_operations_survive_mutated_javascript_globals() {
        let bindings = tools(IndexMap::from([
            ("echo".to_owned(), binding(Ok)),
            ("fail".to_owned(), binding(|_| anyhow::bail!("nope"))),
        ]));
        let outcome = evaluate_program(
            r"
const arrayPrototype = Array.prototype;
const objectPrototype = Object.prototype;
const setPrototype = Set.prototype;
const stringPrototype = String.prototype;
Array.isArray = () => false;
arrayPrototype.at = arrayPrototype.includes = arrayPrototype.pop = arrayPrototype.push = () => { throw new Error('mutated array method') };
Object.defineProperty = Object.getOwnPropertyDescriptor = Object.getPrototypeOf = Object.keys = () => { throw new Error('mutated object method') };
Object.hasOwn = () => false;
Object.is = () => true;
objectPrototype.propertyIsEnumerable = () => false;
Number.isFinite = Number.isSafeInteger = () => false;
Reflect.apply = Reflect.ownKeys = () => { throw new Error('mutated reflect method') };
setPrototype.add = setPrototype.delete = setPrototype.has = () => { throw new Error('mutated set method') };
stringPrototype.charCodeAt = stringPrototype.codePointAt = stringPrototype.slice = () => { throw new Error('mutated string method') };
Buffer.byteLength = () => 0;
Function.prototype.toString = () => 'mutated';
objectPrototype.get = () => undefined;
objectPrototype.constructor = arrayPrototype.constructor = null;
globalThis.Array = globalThis.Buffer = globalThis.Error = globalThis.Function = globalThis.Number = globalThis.Object = globalThis.Reflect = globalThis.Set = globalThis.String = undefined;
const echoed = await tools.echo({ request: ['€', 1] });
let failure;
try { await tools.fail({}) } catch (error) { failure = { typed: error instanceof ToolCallError, name: error.name, toolName: error.toolName, message: error.message }; }
return { echoed, failure, completion: { ok: true, amount: 42 } };
",
            limits(10_000),
            bindings,
        )
        .await
        .unwrap();
        assert!(
            matches!(&outcome.completion, EngineCompletion::Success(Some(value)) if value == &json!({
                "echoed": { "request": ["€", 1] },
                "failure": { "typed": true, "name": "ToolCallError", "toolName": "fail", "message": "nope" },
                "completion": { "ok": true, "amount": 42 }
            })),
            "unexpected completion: {:?}",
            outcome.completion
        );
    }
}
