//! Behavioral mirror of `packages/boot/cmdline/tests/cmdline.spec.ts`.

use std::sync::Arc;

use parking_lot::Mutex;
use seekdeep_cmdline::{
    APP_EXIT, CMDLINE_ARGS, CmdlineHost, CmdlineOutput, CmdlineProgram, CmdlineProgramOutcome,
    parse_cmdline_with_output, provide_cmdline,
};
use seekdeep_cordis::{Context, Plugin, ServiceKey};
use seekdeep_loader::PluginCatalog;

const DEMO_STARTUP: ServiceKey<DemoStartup> = ServiceKey::new("demoStartup");

#[derive(Clone, Debug, PartialEq, Eq)]
struct DemoStartup {
    port: Option<u16>,
}

#[derive(Debug, Default)]
struct RecordingOutput {
    stdout: Mutex<String>,
    stderr: Mutex<String>,
}

impl RecordingOutput {
    fn combined(&self) -> String {
        format!("{}{}", self.stdout.lock(), self.stderr.lock())
    }
}

impl CmdlineOutput for RecordingOutput {
    fn write_stdout(&self, text: &str) -> anyhow::Result<()> {
        self.stdout.lock().push_str(text);
        Ok(())
    }

    fn write_stderr(&self, text: &str) -> anyhow::Result<()> {
        self.stderr.lock().push_str(text);
        Ok(())
    }
}

struct DemoProgram {
    has_action: bool,
    publish: bool,
    fatal: bool,
}

impl DemoProgram {
    const fn publishing() -> Self {
        Self {
            has_action: true,
            publish: true,
            fatal: false,
        }
    }
}

impl CmdlineProgram for DemoProgram {
    type Action = DemoStartup;

    fn name(&self) -> &'static str {
        "demo"
    }

    fn has_action(&self) -> bool {
        self.has_action
    }

    fn parse(&mut self, args: &[String]) -> anyhow::Result<CmdlineProgramOutcome<Self::Action>> {
        if self.fatal {
            anyhow::bail!("action exploded");
        }
        match args {
            [] => Ok(CmdlineProgramOutcome::Action(DemoStartup { port: None })),
            [help] if help == "--help" => Ok(CmdlineProgramOutcome::Exit {
                code: 0,
                stdout: "Usage: demo\n".to_owned(),
                stderr: String::new(),
            }),
            [serve] if serve == "serve" => Ok(CmdlineProgramOutcome::Exit {
                code: 1,
                stdout: String::new(),
                stderr: "error: serve rejected\n".to_owned(),
            }),
            [flag, value] if flag == "--port" => match value.parse::<u16>() {
                Ok(port) => Ok(CmdlineProgramOutcome::Action(DemoStartup {
                    port: Some(port),
                })),
                Err(_) => Ok(CmdlineProgramOutcome::Exit {
                    code: 1,
                    stdout: String::new(),
                    stderr: format!("error: --port must be a number, got {value:?}\n"),
                }),
            },
            _ => anyhow::bail!("unexpected demo argv: {args:?}"),
        }
    }

    fn run_action(&mut self, context: &Context, action: Self::Action) -> anyhow::Result<()> {
        if self.publish {
            context.provide(DEMO_STARTUP, Arc::new(action))?;
        }
        Ok(())
    }
}

fn host(
    context: &Context,
    args: impl IntoIterator<Item = impl Into<String>>,
    exits: &Arc<Mutex<Vec<i32>>>,
) {
    provide_cmdline(
        context,
        CmdlineHost::new(args, {
            let exits = exits.clone();
            move |code| {
                exits.lock().push(code);
                Ok(())
            }
        }),
    )
    .expect("provide cmdline");
}

#[test]
fn snapshot_is_immutable_reusable_and_missing_host_or_action_fails_loud() {
    let context = Context::new();
    let mut caller_args = vec!["--port".to_owned(), "8080".to_owned()];
    let exits = Arc::new(Mutex::new(Vec::new()));
    host(&context, caller_args.clone(), &exits);
    caller_args.push("--tampered".to_owned());
    assert_eq!(
        context.get(CMDLINE_ARGS).expect("args").get(),
        ["--port", "8080"]
    );

    for _ in 0..2 {
        let isolated = context.isolate(DEMO_STARTUP);
        parse_cmdline_with_output(
            &isolated,
            &mut DemoProgram::publishing(),
            &RecordingOutput::default(),
        )
        .expect("parse snapshot");
        assert_eq!(
            isolated.get(DEMO_STARTUP).expect("startup").port,
            Some(8080)
        );
    }
    assert!(exits.lock().is_empty());

    let missing = Context::new();
    let error = parse_cmdline_with_output(
        &missing,
        &mut DemoProgram::publishing(),
        &RecordingOutput::default(),
    )
    .expect_err("missing host");
    assert!(
        error
            .to_string()
            .contains("ctx.cmdlineArgs and ctx.appExit")
    );

    let no_action = Context::new();
    host(
        &no_action,
        std::iter::empty::<String>(),
        &Arc::new(Mutex::new(Vec::new())),
    );
    let error = parse_cmdline_with_output(
        &no_action,
        &mut DemoProgram {
            has_action: false,
            publish: true,
            fatal: false,
        },
        &RecordingOutput::default(),
    )
    .expect_err("missing action");
    assert!(
        error
            .to_string()
            .contains("no command in the program declares an action")
    );
}

#[test]
fn valid_help_action_rejection_and_fatal_paths_preserve_exit_and_publication() {
    let valid = Context::new();
    let valid_exits = Arc::new(Mutex::new(Vec::new()));
    host(&valid, ["--port", "8080"], &valid_exits);
    parse_cmdline_with_output(
        &valid,
        &mut DemoProgram::publishing(),
        &RecordingOutput::default(),
    )
    .expect("valid parse");
    assert_eq!(valid.get(DEMO_STARTUP).expect("startup").port, Some(8080));
    assert!(valid_exits.lock().is_empty());

    for (args, code, text) in [
        (vec!["--help"], 0, "Usage: demo"),
        (vec!["--port", "abc"], 1, "--port must be a number"),
        (vec!["serve"], 1, "serve rejected"),
    ] {
        let context = Context::new();
        let exits = Arc::new(Mutex::new(Vec::new()));
        host(&context, args, &exits);
        let output = RecordingOutput::default();
        parse_cmdline_with_output(&context, &mut DemoProgram::publishing(), &output)
            .expect("terminal parse");
        assert_eq!(&*exits.lock(), &[code]);
        assert!(output.combined().contains(text));
        assert!(context.get(DEMO_STARTUP).is_none());
    }

    let fatal = Context::new();
    host(
        &fatal,
        std::iter::empty::<String>(),
        &Arc::new(Mutex::new(Vec::new())),
    );
    let error = parse_cmdline_with_output(
        &fatal,
        &mut DemoProgram {
            has_action: true,
            publish: true,
            fatal: true,
        },
        &RecordingOutput::default(),
    )
    .expect_err("fatal action");
    assert_eq!(error.to_string(), "action exploded");
    assert!(fatal.get(DEMO_STARTUP).is_none());

    let action_only = Context::new();
    host(
        &action_only,
        std::iter::empty::<String>(),
        &Arc::new(Mutex::new(Vec::new())),
    );
    parse_cmdline_with_output(
        &action_only,
        &mut DemoProgram {
            has_action: true,
            publish: false,
            fatal: false,
        },
        &RecordingOutput::default(),
    )
    .expect("action without service ownership");
    assert!(action_only.get(DEMO_STARTUP).is_none());
}

#[tokio::test]
async fn real_loader_orders_startup_provider_before_dependent_reader() {
    let context = Context::new();
    let exits = Arc::new(Mutex::new(Vec::new()));
    host(&context, ["--port", "8080"], &exits);
    let output = Arc::new(RecordingOutput::default());
    let observed = Arc::new(Mutex::new(None));

    let catalog = PluginCatalog::new();
    let startup_output = output.clone();
    catalog
        .register_named(
            "demo-startup",
            Plugin::new("demo-startup", ["cmdlineArgs"], move |context, _| {
                let output = startup_output.clone();
                Box::pin(async move {
                    parse_cmdline_with_output(
                        &context,
                        &mut DemoProgram::publishing(),
                        output.as_ref(),
                    )?;
                    Ok(())
                })
            }),
        )
        .expect("register startup");
    let reader_observed = observed.clone();
    catalog
        .register_named(
            "reader",
            Plugin::new("reader", ["demoStartup"], move |context, _| {
                let observed = reader_observed.clone();
                Box::pin(async move {
                    *observed.lock() = Some(
                        context
                            .get(DEMO_STARTUP)
                            .ok_or_else(|| anyhow::anyhow!("demoStartup missing"))?
                            .port,
                    );
                    Ok(())
                })
            }),
        )
        .expect("register reader");

    let composition = catalog
        .load_yaml(
            &context,
            concat!(
                "- id: startup\n",
                "  name: demo-startup\n",
                "- id: reader\n",
                "  name: reader\n",
            ),
        )
        .await
        .expect("load composition");
    assert_eq!(*observed.lock(), Some(Some(8080)));
    assert!(exits.lock().is_empty());

    composition.dispose().await.expect("dispose composition");
    context.fiber().dispose().await.expect("dispose root");
    assert!(context.get(CMDLINE_ARGS).is_none());
    assert!(context.get(APP_EXIT).is_none());
}
