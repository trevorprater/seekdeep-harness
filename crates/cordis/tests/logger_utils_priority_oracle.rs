//! Pinned-source comparisons for native logger coercion and exporter lifecycle.

#![cfg(not(target_arch = "wasm32"))]

use std::{
    collections::BTreeMap,
    io::Write as _,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc, OnceLock, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

use parking_lot::Mutex;
use seekdeep_cordis::{
    Context, CordisClock, Fiber, LogExporter, LogMessage, Logger, LoggerLevel, LoggerType,
};
use serde_json::{Value, json};

#[derive(Debug)]
struct FixedClock;

impl CordisClock for FixedClock {
    fn now_ms(&self) -> i64 {
        1234
    }
}

fn source_root() -> &'static PathBuf {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let snapshot = include_str!("../../../SOURCE_SNAPSHOT");
        let field = |name: &str| {
            snapshot
                .lines()
                .find_map(|line| line.strip_prefix(name))
                .expect("source snapshot field")
        };
        let root = PathBuf::from(field("repository="));
        let head = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&root)
            .output()
            .expect("read source revision");
        assert!(head.status.success());
        assert_eq!(
            String::from_utf8(head.stdout).unwrap().trim(),
            field("commit=")
        );
        root
    })
}

fn source(body: &str, input: &Value) -> Value {
    let script = format!(
        "import {{ Context, Logger }} from './vendor/cordis/src/index.ts';\n\
         import {{ readFileSync }} from 'node:fs';\n\
         const input = JSON.parse(readFileSync(0, 'utf8'));\n\
         Date.now = () => 1234;\n{body}\n"
    );
    let mut child = Command::new("node")
        .args([
            "--experimental-transform-types",
            "--input-type=module",
            "--eval",
            &script,
        ])
        .current_dir(source_root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run pinned source");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(serde_json::to_string(input).unwrap().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "source failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("source JSON result")
}

fn message(args: Vec<Value>) -> LogMessage {
    LogMessage {
        sn: 1,
        ts: 1234,
        name: "priority".to_owned(),
        message_type: LoggerType::Info,
        level: LoggerLevel::Info,
        args,
        fiber: Weak::new(),
        meta: serde_json::Map::new(),
    }
}

#[test]
fn formatter_coercion_and_argument_consumption_match_pinned_source() {
    let mut cases = vec![
        json!({"args":[]}),
        json!({"args":["%s %d %i %f %o %O %c %C"]}),
        json!({"args":["%s|%s|%s", [null,1,[null,2]], {}, false]}),
        json!({"args":["%d|%i|%f", " \t12.9\n", "0x10", "0b101"]}),
        json!({"args":["%d|%i|%f", "0o17", "", "\u{feff}\u{a0}"]}),
        json!({"args":["%d|%i|%f", [], [null], ["2.8"]]}),
        json!({"args":["%d|%i|%f", [1,2], {}, true]}),
        json!({"args":["%d|%i|%f", "Infinity", "-Infinity", "+Infinity"]}),
        json!({"args":["%f|%f|%f", "inf", "infinity", "1_000"]}),
        json!({"args":["%f|%f|%f", "-0x1", "0b2", ".25"]}),
        json!({"args":["%f|%f|%f", "1e+21", "1e-7", "-0"]}),
        json!({"args":["%s|%o", 1e21, {"large":1e21,"small":1e-7,"zero":-0.0}]}),
        json!({"args":["%o", {"10":"ten","2":"two","01":"one","0":"zero"}]}),
        json!({"args":["tail", {"value":1}, [2,3], null], "objectFormatter":true}),
        json!({"args":["%o|%O", {"value":1}, {"value":2}], "objectFormatter":true}),
        json!({"args":["%o|%o", null], "objectFormatter":true}),
        json!({"args":[], "objectFormatter":true}),
        json!({"args":["%1|%é|%q|%%|%s", "retained"], "nonLetterFormatter":true}),
        json!({"args":["one\r\ntwo\nlast\r"]}),
        json!({"args":["🚀start\r\nlong"],"maxLength":1}),
        json!({"args":["🚀start\r\nlong"],"maxLength":2}),
        json!({"args":["zero\n"],"maxLength":0}),
    ];
    for literal in [
        "0x20000000000001",
        "0x20000000000003",
        "0x123456789abcdef123456789abcdef123456789abcdef",
        "0b1000000000000000000000000000000000000000000000000000011",
        "0o777777777777777777777777777777777777777777777777777777777777",
    ] {
        cases.push(json!({"args":["%f", literal]}));
    }
    cases.push(serde_json::from_str(r#"{"args":["%f|%s|%o",1e400,-1e400,1e400]}"#).unwrap());
    let expected = source(
        "const result = input.map(({ args, maxLength, objectFormatter, nonLetterFormatter }) => {\n\
           const exporter = { maxLength, formatters: {} };\n\
           if (objectFormatter) exporter.formatters.o = value => '[' + JSON.stringify(value) + ']';\n\
           if (nonLetterFormatter) Object.assign(exporter.formatters, { '1': () => 'bad', 'é': () => 'bad' });\n\
           return Buffer.from(Logger.format(exporter, { name:'priority', args }), 'utf8').toString('utf8');\n\
         });\nconsole.log(JSON.stringify(result));",
        &json!(cases),
    );
    let actual: Vec<String> = cases
        .iter()
        .map(|case| {
            let mut exporter = LogExporter::new(|_| {});
            if let Some(length) = case["maxLength"].as_u64() {
                exporter.max_length = usize::try_from(length).unwrap();
            }
            if case["objectFormatter"] == true {
                exporter.formatters.insert(
                    'o',
                    Arc::new(|value, _, _| {
                        format!(
                            "[{}]",
                            value.map_or_else(
                                || "undefined".to_owned(),
                                |value| serde_json::to_string(value).unwrap()
                            )
                        )
                    }),
                );
            }
            if case["nonLetterFormatter"] == true {
                for key in ['1', 'é'] {
                    exporter
                        .formatters
                        .insert(key, Arc::new(|_, _, _| "bad".to_owned()));
                }
            }
            Logger::format(
                &exporter,
                &message(case["args"].as_array().unwrap().clone()),
            )
        })
        .collect();
    assert_eq!(json!(actual), expected);
}

#[test]
fn buffer_size_assignment_and_zero_retention_match_pinned_source() {
    let expected = source(
        "const ctx = new Context(), result = [];\n\
         const record = () => result.push(ctx.logger.buffer.map(message => message.args[0]));\n\
         ctx.logger.info('first'); ctx.logger.info('second');\n\
         ctx.logger.bufferSize = 1; record();\n\
         ctx.logger.info('third'); record();\n\
         ctx.logger.bufferSize = 0; record();\n\
         ctx.logger.info('fourth'); record();\n\
         console.log(JSON.stringify(result));",
        &Value::Null,
    );
    let context = Context::new_with_clock(Arc::new(FixedClock));
    let logger = context.logger(None);
    let record = || {
        context
            .logger_service()
            .buffer()
            .iter()
            .map(|message| message.args[0].clone())
            .collect::<Vec<_>>()
    };
    logger.info([json!("first")]);
    logger.info([json!("second")]);
    context.logger_service().set_buffer_size(1);
    let mut actual = vec![record()];
    logger.info([json!("third")]);
    actual.push(record());
    context.logger_service().set_buffer_size(0);
    actual.push(record());
    logger.info([json!("fourth")]);
    actual.push(record());
    assert_eq!(json!(actual), expected);
}

#[test]
fn fractional_and_unbounded_numeric_intercept_levels_match_pinned_source() {
    let levels: Value = serde_json::from_str("[2.5,0.5,-0.5,10,-10,1e400,-1e400]").unwrap();
    let expected = source(
        "const result = input.map(level => {\n\
           const ctx = new Context(), logger = ctx.intercept('logger', { level }).logger();\n\
           for (const type of ['error', 'info', 'warn', 'debug']) logger[type](type);\n\
           return ctx.logger.buffer.map(message => message.type);\n\
         }); console.log(JSON.stringify(result));",
        &levels,
    );
    let actual: Vec<_> = levels
        .as_array()
        .unwrap()
        .iter()
        .map(|level| {
            let context = Context::new_with_clock(Arc::new(FixedClock));
            let scoped = context.intercept("logger", json!({"level": level}));
            let logger = scoped.logger(None);
            logger.error([json!("error")]);
            logger.info([json!("info")]);
            logger.warn([json!("warn")]);
            logger.debug([json!("debug")]);
            context
                .logger_service()
                .buffer()
                .iter()
                .map(|message| message.message_type.as_str())
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(json!(actual), expected);
}

#[tokio::test]
async fn exporter_disposal_uses_the_current_counter_like_pinned_source() {
    let expected = source(
        "const ctx = new Context(), result = [];\n\
         const first = ctx.logger.exporter({ export: message => result.push(['first',message.args[0]]) });\n\
         const second = ctx.logger.exporter({ export: message => result.push(['second',message.args[0]]) });\n\
         first(); second(); ctx.logger.info('retained');\n\
         console.log(JSON.stringify(result));",
        &Value::Null,
    );
    let context = Context::new_with_clock(Arc::new(FixedClock));
    let actual = Arc::new(Mutex::new(Vec::new()));
    let make_exporter = |name| {
        let output = actual.clone();
        LogExporter::new(move |message| output.lock().push(json!([name, message.args[0]])))
    };
    let first = context
        .logger_service()
        .exporter(&context, make_exporter("first"))
        .unwrap();
    let second = context
        .logger_service()
        .exporter(&context, make_exporter("second"))
        .unwrap();
    first.dispose().await.unwrap();
    second.dispose().await.unwrap();
    context.logger(None).info([json!("retained")]);
    assert_eq!(json!(*actual.lock()), expected);
}

#[tokio::test]
async fn inactive_exporter_admission_does_not_advance_the_disposal_counter() {
    let context = Context::new_with_clock(Arc::new(FixedClock));
    let calls = Arc::new(Mutex::new(0));
    let output = calls.clone();
    let registered = context
        .logger_service()
        .exporter(&context, LogExporter::new(move |_| *output.lock() += 1))
        .unwrap();
    let disposed = Fiber::active_child("disposed");
    disposed.dispose().await.unwrap();
    let inactive = context.with_fiber(disposed);
    assert!(
        context
            .logger_service()
            .exporter(&inactive, LogExporter::new(|_| {}))
            .is_err()
    );
    registered.dispose().await.unwrap();
    context.logger(None).info([json!("removed")]);
    assert_eq!(*calls.lock(), 0);
}

#[tokio::test]
async fn exporters_added_during_emission_receive_the_same_record() {
    let expected = source(
        "const ctx = new Context(), result = []; let registered = false;\n\
         ctx.logger.exporter({ export(message) {\n\
           result.push(['first',message.sn,message.ts]);\n\
           if (!registered) { registered = true; ctx.logger.exporter({ export(message) { result.push(['late',message.sn,message.ts]); } }); }\n\
         } });\n\
         ctx.logger.info('message'); console.log(JSON.stringify(result));",
        &Value::Null,
    );
    let context = Context::new_with_clock(Arc::new(FixedClock));
    let result = Arc::new(Mutex::new(Vec::new()));
    let first_output = result.clone();
    let registration_context = context.clone();
    let registered = AtomicBool::new(false);
    let mut exporter = LogExporter::new(move |message| {
        first_output
            .lock()
            .push(json!(["first", message.sn, message.ts]));
        if !registered.swap(true, Ordering::AcqRel) {
            let late_output = first_output.clone();
            registration_context
                .logger_service()
                .exporter(
                    &registration_context,
                    LogExporter::new(move |message| {
                        late_output
                            .lock()
                            .push(json!(["late", message.sn, message.ts]));
                    }),
                )
                .unwrap();
        }
    });
    exporter.levels = BTreeMap::from([("default".to_owned(), 3)]);
    context
        .logger_service()
        .exporter(&context, exporter)
        .unwrap();
    context.logger(None).info([json!("message")]);
    assert_eq!(json!(*result.lock()), expected);
    context.fiber().dispose().await.unwrap();
}
