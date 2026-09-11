//! Source/native Host reload publication, provider visibility, and lifecycle ordering.

use std::{
    fs::OpenOptions,
    io::Write as _,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventOptions, EventReply, FiberState, PluginFiber};
use seekdeep_loader::{LOADER, PluginCatalog};
use serde_json::{Value, json};

fn record(path: &Path, value: &Value) -> anyhow::Result<()> {
    let mut encoded = serde_json::to_vec(value)?;
    encoded.push(b'\n');
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(&encoded)?;
    Ok(())
}

fn trace(path: &Path) -> anyhow::Result<Value> {
    std::fs::read_to_string(path)?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<Vec<_>, _>>()
        .map(Value::Array)
        .map_err(Into::into)
}

const fn state(state: FiberState) -> u8 {
    match state {
        FiberState::Pending => 0,
        FiberState::Loading => 1,
        FiberState::Active => 2,
        FiberState::Failed => 3,
        FiberState::Disposed => 4,
        FiberState::Unloading => 5,
    }
}

fn plugin(generation: &str, failure: &str, async_dispose: bool) -> String {
    format!(
        "import {{ appendFileSync }} from 'node:fs';\n\
         const record = kind => appendFileSync(new URL('trace.jsonl', import.meta.url), JSON.stringify({{kind}}) + '\\n');\n\
         export const name = {generation:?};\n\
         export const generation = {generation:?};\n\
         export function apply(ctx, config) {{\n\
           record('apply:' + generation + ':' + config.id);\n\
           ctx.provide('probe_' + config.id, generation);\n\
           ctx.effect(() => {asynchronous}() => {{\n\
             record('dispose-start:' + generation + ':' + config.id);\n\
             {wait}\n\
             record('dispose-end:' + generation + ':' + config.id);\n\
           }});\n\
           if ({failure:?} === 'body' && config.id === 'second') throw new Error('second replacement apply rejected');\n\
         }}\napply.generation = generation;\n",
        asynchronous = if async_dispose { "async " } else { "" },
        wait = if async_dispose {
            "await Promise.resolve();"
        } else {
            ""
        },
    )
}

#[derive(Clone)]
struct Observation {
    context: Context,
    trace: PathBuf,
    first_uid: Arc<Mutex<Option<u64>>>,
    old: Arc<Mutex<Vec<Arc<PluginFiber>>>>,
    reject: Arc<AtomicBool>,
}

impl Observation {
    fn uid(&self, fiber: &PluginFiber) -> Option<u64> {
        fiber.uid().map(|uid| uid - self.first_uid.lock().unwrap())
    }

    fn fiber(&self, fiber: &PluginFiber) -> Value {
        json!({"id":fiber.entry_id(), "uid":self.uid(fiber), "state":state(fiber.fiber().state())})
    }

    fn provider(&self, id: &str) -> Value {
        let name = format!("probe_{id}");
        json!({"id":id,
            "strict":self.context.get_named::<Value>(&name).as_deref(),
            "loose":self.context.get_named_relaxed::<Value>(&name).as_deref()})
    }

    fn publication(&self, fiber: &PluginFiber) -> anyhow::Result<()> {
        let config = fiber.config();
        let Some(id @ ("first" | "second")) = config["id"].as_str() else {
            return Ok(());
        };
        {
            let mut first_uid = self.first_uid.lock();
            if first_uid.is_none() {
                *first_uid = fiber.uid();
            }
        }
        let name = format!("probe_{id}");
        record(
            &self.trace,
            &json!({"kind":"publication", "id":id, "generation":fiber.plugin_name(),
            "uid":self.uid(fiber), "state":state(fiber.fiber().state()),
            "strict":self.context.get_named::<Value>(&name).as_deref(),
            "loose":self.context.get_named_relaxed::<Value>(&name).as_deref()}),
        )?;
        if fiber.uid().is_some() && id == "second" && self.reject.swap(false, Ordering::AcqRel) {
            record(&self.trace, &json!({"kind":"registration-failure:second"}))?;
            anyhow::bail!("second replacement registration rejected");
        }
        Ok(())
    }

    fn reload(&self) -> anyhow::Result<()> {
        let entries = self
            .context
            .registry()
            .values()
            .into_iter()
            .flat_map(|runtime| runtime.fibers)
            .filter(|fiber| matches!(fiber.entry_id().as_deref(), Some("first" | "second")))
            .map(|fiber| self.fiber(&fiber))
            .collect::<Vec<_>>();
        record(
            &self.trace,
            &json!({"kind":"event:hmr/reload",
            "oldFibers":self.old.lock().iter().map(|fiber|self.fiber(fiber)).collect::<Vec<_>>(),
            "entries":entries,
            "providers":[self.provider("first"),self.provider("second")]}),
        )
    }

    fn install(&self) -> anyhow::Result<()> {
        let observation = self.clone();
        self.context.events().on(
            &self.context,
            "internal/plugin",
            move |_, args| {
                let fiber = args.get::<PluginFiber>(0).unwrap();
                let result = observation.publication(&fiber);
                Box::pin(async move {
                    result?;
                    Ok(EventReply::Undefined)
                })
            },
            EventOptions::default(),
        )?;
        let observation = self.clone();
        self.context.events().on(
            &self.context,
            "hmr/reload",
            move |_, _| {
                let result = observation.reload();
                Box::pin(async move {
                    result?;
                    Ok(EventReply::Undefined)
                })
            },
            EventOptions::default(),
        )?;
        Ok(())
    }
}

async fn scenario(failure: &str, async_dispose: bool) -> anyhow::Result<Value> {
    let temporary = tempfile::tempdir()?;
    let filename = temporary.path().join("plugin.mjs");
    let tracefile = temporary.path().join("trace.jsonl");
    let context = Context::new();
    let observation = Observation {
        context: context.clone(),
        trace: tracefile.clone(),
        first_uid: Arc::default(),
        old: Arc::default(),
        reject: Arc::new(AtomicBool::new(false)),
    };
    observation.install()?;
    std::fs::write(&filename, plugin("old", "none", async_dispose))?;
    let composition = PluginCatalog::new().load_yaml_at(
        &context,
        "- id: first\n  name: ./plugin.mjs\n  config: { id: first }\n- id: second\n  name: ./plugin.mjs\n  config: { id: second }\n",
        temporary.path().join("cordis.yml"),
    ).await?;
    *observation.old.lock() = composition.fibers();
    let initial = trace(&tracefile)?;
    std::fs::write(&tracefile, "")?;
    std::fs::write(&filename, plugin("new", failure, async_dispose))?;
    observation
        .reject
        .store(failure == "registration", Ordering::Release);
    let replaced = composition.reload_module(&filename).await;
    if failure == "registration" {
        assert!(
            replaced
                .unwrap_err()
                .to_string()
                .contains("second replacement registration rejected")
        );
    } else {
        replaced?;
    }
    let settlement_error = context
        .get(LOADER)
        .unwrap()
        .wait()
        .await
        .err()
        .map(|error| error.to_string());
    let old = observation.old.lock().clone();
    PluginFiber::await_all_quiescent(&old).await;
    let settled = trace(&tracefile)?;
    let snapshots = composition.entries();
    let entries = composition
        .fibers()
        .into_iter()
        .map(|fiber| {
            let id = fiber.entry_id().unwrap();
            let snapshot = snapshots
                .iter()
                .find(|entry| entry.id.as_str() == id)
                .unwrap();
            json!({"id":id,"uid":observation.uid(&fiber),"state":state(fiber.fiber().state()),
            "disabled":snapshot.disabled,"generation":fiber.plugin_name()})
        })
        .collect::<Vec<_>>();
    composition.dispose().await?;
    Ok(
        json!({"failure":failure,"asyncDispose":async_dispose,"initial":initial,"settled":settled,
        "entries":entries,"settlementError":settlement_error}),
    )
}

fn source_reports() -> anyhow::Result<Value> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = std::process::Command::new("node")
        .args(["--experimental-transform-types", "--expose-internals"])
        .arg(root.join("tests/fixtures/hmr_lifecycle_oracle.mjs"))
        .arg(root.join("../../../deepseek-harness"))
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

#[tokio::test]
async fn real_node_reload_matches_all_six_source_lifecycle_traces() -> anyhow::Result<()> {
    let expected = source_reports()?;
    let mut actual = Vec::new();
    for async_dispose in [false, true] {
        for failure in ["none", "body", "registration"] {
            actual.push(
                tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    scenario(failure, async_dispose),
                )
                .await??,
            );
        }
    }
    let actual = Value::Array(actual);
    if let Some(directory) = std::env::var_os("SEEKDEEP_HMR_TRACE_DIR") {
        let directory = PathBuf::from(directory);
        std::fs::create_dir_all(&directory)?;
        std::fs::write(
            directory.join("source-hmr-lifecycle.json"),
            serde_json::to_vec_pretty(&expected)?,
        )?;
        std::fs::write(
            directory.join("native-hmr-lifecycle.json"),
            serde_json::to_vec_pretty(&actual)?,
        )?;
    }
    assert_eq!(actual, expected);
    Ok(())
}
