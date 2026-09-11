//! Import failures crossing the JavaScript runtime, Loader, and real Host watcher.

use std::{
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use seekdeep_cordis::{
    Context, ServiceKey,
    logger::{LogExporter, LoggerLevel},
};
use seekdeep_hmr::{Config, HostHmrService, error::handle_loader_error};
use seekdeep_loader::{LOADER, LoaderError, PluginCatalog};
use serde_json::{Value, json};

const VALUE: ServiceKey<Value> = ServiceKey::new("structuredErrorValue");

fn test_context() -> Context {
    Context::new().intercept("logger", json!({"level":3}))
}

fn source_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../deepseek-harness")
}

fn source_warnings(error: &Value) -> anyhow::Result<Value> {
    source_warnings_with_replacement(error, None)
}

fn source_warnings_with_replacement(
    error: &Value,
    replacement: Option<(&Path, &str)>,
) -> anyhow::Result<Value> {
    let mut child = Command::new("node")
        .args([
            "--input-type=module",
            "-e",
            concat!(
                "import fs from 'node:fs'; ",
                "import { handleError } from './vendor/hmr/src/error.ts'; ",
                "const { error, replacement } = JSON.parse(fs.readFileSync(0, 'utf8')); ",
                "const warnings = []; ",
                "try { ",
                "  handleError({ logger: { warn(value) { ",
                "    warnings.push(value); ",
                "    if (warnings.length === 1 && replacement) fs.writeFileSync(...replacement); ",
                "  } } }, error); ",
                "  process.stdout.write(JSON.stringify({ warnings })); ",
                "} catch (failure) { ",
                "  process.stdout.write(JSON.stringify({ error: failure.message })); ",
                "}",
            ),
        ])
        .env("FORCE_COLOR", "0")
        .env_remove("NO_COLOR")
        .current_dir(source_root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(&serde_json::to_vec(
            &json!({"error":error,"replacement":replacement}),
        )?)?;
    let output = child.wait_with_output()?;
    anyhow::ensure!(
        output.status.success(),
        "source handler failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn warnings(context: &Context) -> Vec<Value> {
    context
        .logger_service()
        .buffer()
        .into_iter()
        .filter(|message| message.level == LoggerLevel::Warn)
        .flat_map(|message| message.args)
        .map(|value| match value {
            Value::String(value) => Value::String(without_ansi(&value)),
            value => value,
        })
        .collect()
}

fn without_ansi(value: &str) -> String {
    let mut output = String::new();
    let mut characters = value.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\u{1b}' && characters.peek() == Some(&'[') {
            characters.next();
            for character in characters.by_ref() {
                if ('@'..='~').contains(&character) {
                    break;
                }
            }
        } else {
            output.push(character);
        }
    }
    output
}

async fn eventually(mut predicate: impl FnMut() -> bool, context: &Context) -> anyhow::Result<()> {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "watcher condition timed out; warnings: {:?}",
            warnings(context)
        )
    })
}

fn valid_plugin(value: &str) -> String {
    format!(
        "export function apply(ctx) {{ ctx.provide('structuredErrorValue', {}); }}\n",
        json!(value)
    )
}

fn start_watcher(context: &Context, root: &Path) -> anyhow::Result<Arc<HostHmrService>> {
    HostHmrService::start(
        context.clone(),
        context.get(LOADER).expect("attached loader"),
        Config {
            base: Some(root.to_owned()),
            root: vec![root.to_owned()],
            debounce: 20,
            ignored: Vec::new(),
            watcher_options: serde_json::Map::from_iter([
                ("usePolling".to_owned(), json!(true)),
                ("interval".to_owned(), json!(80)),
            ]),
        },
        Arc::new(|| Box::pin(async { Ok(()) })),
    )
}

#[test]
fn loader_failure_transport_preserves_source_warning_arguments_and_predicate_failures()
-> anyhow::Result<()> {
    for error in [
        Value::Null,
        json!("a thrown string"),
        json!({"name":"TypeError","message":"ordinary import failure","stack":"TypeError: ordinary import failure\n    at plugin.mjs:1:1"}),
        json!({"errors":[]}),
        json!({"errors":[{"text":""}],"message":"keep this error object"}),
        json!({"errors":[{"text":"first"},{"text":23},{"text":true}]}),
        json!({"errors":[null]}),
    ] {
        let expected = source_warnings(&error)?;
        let context = test_context();
        let failure = LoaderError::StructuredModuleLoad {
            message: "text-only fallback".into(),
            error: error.clone(),
        };
        assert_eq!(failure.structured_error(), Some(&error));
        let actual = match handle_loader_error(&context, &failure) {
            Ok(()) => json!({"warnings":warnings(&context)}),
            Err(error) => json!({"error":error.to_string()}),
        };
        assert_eq!(actual, expected, "payload: {error}");
    }
    let context = test_context();
    let failure = LoaderError::ModuleLoad("plain import failure".into());
    assert!(failure.structured_error().is_none());
    handle_loader_error(&context, &failure)?;
    assert_eq!(warnings(&context), [json!("plain import failure")]);
    Ok(())
}

#[tokio::test]
async fn source_logger_callbacks_observe_each_diagnostic_before_the_next_file_read()
-> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let file = temporary.path().join("plugin.ts");
    let before = "const marker = 'before';\n";
    let after = "const marker = 'after';\n";
    std::fs::write(&file, before)?;
    let error = json!({"errors":[
        {"text":"first diagnostic","location":{"file":file,"line":1,"column":1}},
        {"text":"second diagnostic","location":{"file":file,"line":1,"column":1}},
    ]});
    let expected = source_warnings_with_replacement(&error, Some((&file, after)))?;
    assert!(
        expected["warnings"][0]
            .as_str()
            .unwrap()
            .contains("'before'")
    );
    assert!(
        expected["warnings"][1]
            .as_str()
            .unwrap()
            .contains("'after'")
    );
    std::fs::write(&file, before)?;

    let context = test_context();
    let changed = Arc::new(AtomicBool::new(false));
    let exporter = context.logger_service().exporter(
        &context,
        LogExporter::new({
            let changed = changed.clone();
            move |message| {
                if message.level == LoggerLevel::Warn && !changed.swap(true, Ordering::AcqRel) {
                    std::fs::write(&file, after).expect("replace diagnostic source");
                }
            }
        }),
    )?;
    handle_loader_error(
        &context,
        &LoaderError::StructuredModuleLoad {
            message: "two compiler diagnostics".into(),
            error,
        },
    )?;
    assert!(changed.load(Ordering::Acquire));
    assert_eq!(json!({"warnings":warnings(&context)}), expected);
    exporter.dispose().await?;
    Ok(())
}

#[tokio::test]
async fn real_compiler_import_failure_reaches_watcher_code_frames_and_recovers()
-> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let module = temporary.path().join("plugin.mjs");
    let broken = temporary.path().join("broken.ts");
    std::fs::write(&module, valid_plugin("stable"))?;
    std::fs::write(&broken, "const valid: number = 1;\nconst broken = ;\n")?;
    let context = test_context();
    let composition = PluginCatalog::new()
        .load_yaml_at(
            &context,
            "- id: plugin\n  name: ./plugin.mjs\n",
            &temporary.path().join("cordis.yml"),
        )
        .await?;
    let failing_plugin = format!(
        concat!(
            "import {{ createRequire }} from 'node:module';\n",
            "import {{ readFileSync }} from 'node:fs';\n",
            "const require = createRequire({oracle_package});\n",
            "const {{ transformSync }} = require('esbuild');\n",
            "try {{ transformSync(readFileSync({broken}, 'utf8'), {{ loader: 'ts', sourcefile: {broken} }}); }}\n",
            "catch (error) {{ error.errors.push({{ text: 'secondary compiler diagnostic' }}); throw error; }}\n",
            "export function apply() {{}}\n",
        ),
        oracle_package = json!(source_root().join("vendor/hmr/package.json")),
        broken = json!(broken),
    );
    std::fs::write(&module, &failing_plugin)?;
    let failure = context
        .get(LOADER)
        .expect("attached loader")
        .reload_modules(std::slice::from_ref(&module))
        .await
        .expect_err("esbuild must reject the malformed TypeScript");
    let captured = failure
        .structured_error()
        .unwrap_or_else(|| panic!("compiler error lost its fields: {failure:?}"));
    assert_eq!(captured["errors"].as_array().map(Vec::len), Some(2));
    assert_eq!(captured["errors"][0]["location"]["line"], 2);
    assert_eq!(captured["errors"][0]["location"]["column"], 15);
    assert_eq!(captured["errors"][0]["location"]["file"], json!(broken));
    let expected = source_warnings(captured)?["warnings"]
        .as_array()
        .expect("source warnings")
        .clone();
    assert!(expected[0].as_str().is_some_and(|value| {
        value.starts_with("File: ")
            && value.contains("const broken = ;")
            && value.contains("Unexpected")
            && value.contains('^')
    }));
    assert_eq!(expected[1], "secondary compiler diagnostic");
    assert_eq!(context.get(VALUE).as_deref(), Some(&json!("stable")));

    let watcher = start_watcher(&context, temporary.path())?;
    std::fs::write(&module, format!("{failing_plugin}\n"))?;
    eventually(
        || {
            warnings(&context)
                .windows(expected.len())
                .any(|values| values == expected)
        },
        &context,
    )
    .await?;
    assert_eq!(context.get(VALUE).as_deref(), Some(&json!("stable")));
    std::fs::write(&module, valid_plugin("recovered"))?;
    eventually(
        || context.get(VALUE).as_deref() == Some(&json!("recovered")),
        &context,
    )
    .await?;
    watcher.dispose().await?;
    composition.dispose().await?;
    Ok(())
}

#[tokio::test]
async fn ordinary_import_errors_keep_their_fields_in_watcher_logs() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let module = temporary.path().join("plugin.mjs");
    std::fs::write(&module, valid_plugin("stable"))?;
    let context = test_context();
    let composition = PluginCatalog::new()
        .load_yaml_at(
            &context,
            "- id: plugin\n  name: ./plugin.mjs\n",
            &temporary.path().join("cordis.yml"),
        )
        .await?;
    let watcher = start_watcher(&context, temporary.path())?;
    std::fs::write(
        &module,
        concat!(
            "const error = new TypeError('ordinary import rejection');\n",
            "error.code = 'E_HMR_IMPORT';\n",
            "error.errors = [{ text: '' }];\n",
            "throw error;\n",
            "export function apply() {}\n",
        ),
    )?;
    eventually(
        || {
            warnings(&context)
                .iter()
                .any(|value| value["message"] == "ordinary import rejection")
        },
        &context,
    )
    .await?;
    let error = warnings(&context)
        .into_iter()
        .find(|value| value["message"] == "ordinary import rejection")
        .expect("original error warning");
    assert_eq!(error["name"], "TypeError");
    assert_eq!(error["code"], "E_HMR_IMPORT");
    assert_eq!(error["errors"], json!([{"text":""}]));
    assert!(error["stack"].as_str().is_some_and(|stack| {
        stack.contains("TypeError: ordinary import rejection") && stack.contains("plugin.mjs")
    }));
    assert_eq!(context.get(VALUE).as_deref(), Some(&json!("stable")));
    for thrown in [
        json!("string import rejection"),
        Value::Null,
        json!(["array import rejection", 23]),
    ] {
        std::fs::write(
            &module,
            format!("throw {thrown};\nexport function apply() {{}}\n"),
        )?;
        eventually(|| warnings(&context).contains(&thrown), &context).await?;
        assert_eq!(context.get(VALUE).as_deref(), Some(&json!("stable")));
    }
    std::fs::write(&module, valid_plugin("recovered"))?;
    eventually(
        || context.get(VALUE).as_deref() == Some(&json!("recovered")),
        &context,
    )
    .await?;
    watcher.dispose().await?;
    composition.dispose().await?;
    Ok(())
}
