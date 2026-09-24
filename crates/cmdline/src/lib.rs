//! Launcher-owned command-line facts and app-owned parsing.
//!
//! The launcher publishes an immutable inner-argument snapshot and one bounded
//! exit request before an application tree mounts. App plugins parse that same
//! snapshot with their own native grammar, publish services only from a valid
//! action, and route help or usage exits through the launcher controller.

use std::{fmt, io::Write as _, sync::Arc};

use seekdeep_cordis::{Context, ServiceKey};
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};

/// Typed Cordis slot for the immutable inner argument snapshot.
pub const CMDLINE_ARGS: ServiceKey<CmdlineArgs> = ServiceKey::new("cmdlineArgs");
/// Typed Cordis slot for the launcher-owned bounded exit request.
pub const APP_EXIT: ServiceKey<AppExit> = ServiceKey::new("appExit");
/// Cordis companion plugin name from the source package.
pub const INVARIANT_PLUGIN_NAME: &str = "cmdline-invariant";
/// Service required before the invariant companion can register.
pub const INVARIANT_INJECT: &[&str] = &["invariants"];
/// Package identity reserved in the invariant registry.
pub const INVARIANT_PACKAGE: &str = "seekdeep-cmdline";

/// Immutable inner arguments handed from the launcher to every app parser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CmdlineArgs {
    values: Arc<[String]>,
}

impl CmdlineArgs {
    /// Copies an argument sequence into one immutable shared snapshot.
    #[must_use]
    pub fn new<I, S>(values: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            values: values
                .into_iter()
                .map(Into::into)
                .collect::<Vec<_>>()
                .into(),
        }
    }

    /// Read the arguments in their original order.
    #[must_use]
    pub fn get(&self) -> &[String] {
        &self.values
    }
}

type ExitCallback = Arc<dyn Fn(i32) -> anyhow::Result<()> + Send + Sync>;

/// Cloneable launcher callback that starts bounded whole-application exit.
#[derive(Clone)]
pub struct AppExit {
    callback: ExitCallback,
}

impl AppExit {
    /// Wraps one host-owned exit request.
    #[must_use]
    pub fn new(callback: impl Fn(i32) -> anyhow::Result<()> + Send + Sync + 'static) -> Self {
        Self {
            callback: Arc::new(callback),
        }
    }

    /// Requests exit after the host's bounded teardown policy runs.
    ///
    /// # Errors
    ///
    /// Returns a host callback failure.
    pub fn request(&self, code: i32) -> anyhow::Result<()> {
        (self.callback)(code)
    }
}

impl fmt::Debug for AppExit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("AppExit").finish_non_exhaustive()
    }
}

/// Launcher facts installed before any application entry mounts.
#[derive(Clone, Debug)]
pub struct CmdlineHost {
    /// Immutable inner arguments.
    pub args: CmdlineArgs,
    /// Bounded application exit request.
    pub exit: AppExit,
}

impl CmdlineHost {
    /// Copies arguments and wraps one host exit callback.
    #[must_use]
    pub fn new<I, S>(
        args: I,
        exit: impl Fn(i32) -> anyhow::Result<()> + Send + Sync + 'static,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            args: CmdlineArgs::new(args),
            exit: AppExit::new(exit),
        }
    }
}

/// Publishes both launcher facts as effects owned by `context`.
///
/// # Errors
///
/// Returns duplicate-service or inactive-owner failures.
pub fn provide_cmdline(context: &Context, host: CmdlineHost) -> anyhow::Result<()> {
    context.provide(CMDLINE_ARGS, Arc::new(host.args))?;
    context.provide(APP_EXIT, Arc::new(host.exit))?;
    Ok(())
}

/// Terminal control-flow result produced by an app's native parser.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CmdlineProgramOutcome<Action> {
    /// A valid parse selected an action; only this path may publish app state.
    Action(Action),
    /// Help, version, or a usage error requested process exit.
    Exit {
        /// Requested process status.
        code: i32,
        /// Text routed to standard output before exit.
        stdout: String,
        /// Text routed to standard error before exit.
        stderr: String,
    },
}

/// Native app grammar consumed through [`parse_cmdline`].
pub trait CmdlineProgram {
    /// Values resolved by a successful parse and passed to the action.
    type Action;

    /// Stable command name used in host-precondition diagnostics.
    fn name(&self) -> &str;

    /// Whether this program or one of its subcommands declares an action.
    fn has_action(&self) -> bool;

    /// Parses one immutable invocation snapshot.
    ///
    /// # Errors
    ///
    /// Returns a non-control-flow parser or action-preparation failure.
    fn parse(&mut self, args: &[String]) -> anyhow::Result<CmdlineProgramOutcome<Self::Action>>;

    /// Runs the valid action that may publish the app-owned service.
    ///
    /// # Errors
    ///
    /// Returns an action failure. Invalid/help parses never call this method.
    fn run_action(&mut self, context: &Context, action: Self::Action) -> anyhow::Result<()>;
}

/// Sink used to route an app parser's help and diagnostics.
pub trait CmdlineOutput: Send + Sync {
    /// Writes command-owned standard output.
    ///
    /// # Errors
    ///
    /// Returns the underlying output failure.
    fn write_stdout(&self, text: &str) -> anyhow::Result<()>;

    /// Writes command-owned standard error.
    ///
    /// # Errors
    ///
    /// Returns the underlying output failure.
    fn write_stderr(&self, text: &str) -> anyhow::Result<()>;
}

#[derive(Clone, Copy, Debug, Default)]
struct ProcessOutput;

impl CmdlineOutput for ProcessOutput {
    fn write_stdout(&self, text: &str) -> anyhow::Result<()> {
        std::io::stdout().lock().write_all(text.as_bytes())?;
        Ok(())
    }

    fn write_stderr(&self, text: &str) -> anyhow::Result<()> {
        std::io::stderr().lock().write_all(text.as_bytes())?;
        Ok(())
    }
}

/// Parses an app program using the process streams and launcher services.
///
/// # Errors
///
/// Returns when launcher services are absent, the program declares no action,
/// output fails, the exit request fails, or the parser/action returns a fatal
/// non-control-flow failure.
pub fn parse_cmdline<P: CmdlineProgram>(context: &Context, program: &mut P) -> anyhow::Result<()> {
    parse_cmdline_with_output(context, program, &ProcessOutput)
}

/// Parses an app program with an injectable output sink.
///
/// # Errors
///
/// Returns the same failures as [`parse_cmdline`].
pub fn parse_cmdline_with_output<P: CmdlineProgram>(
    context: &Context,
    program: &mut P,
    output: &dyn CmdlineOutput,
) -> anyhow::Result<()> {
    let args = context.get(CMDLINE_ARGS);
    let exit = context.get(APP_EXIT);
    let (Some(args), Some(exit)) = (args, exit) else {
        anyhow::bail!(
            "{}: the launcher must provide ctx.cmdlineArgs and ctx.appExit before the tree mounts",
            program.name()
        );
    };
    anyhow::ensure!(
        program.has_action(),
        "{}: no command in the program declares an action; parseCmdline runs the invoked command's action on a successful parse, and app code there publishes its service",
        program.name()
    );
    match program.parse(args.get())? {
        CmdlineProgramOutcome::Action(action) => program.run_action(context, action),
        CmdlineProgramOutcome::Exit {
            code,
            stdout,
            stderr,
        } => {
            output.write_stdout(&stdout)?;
            output.write_stderr(&stderr)?;
            exit.request(code)
        }
    }
}

/// Registers the package's intentionally empty invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(INVARIANT_PACKAGE, InvariantInstaller::noop())
}

#[cfg(test)]
mod tests {
    use parking_lot::Mutex;
    use seekdeep_cordis::ServiceKey;
    use seekdeep_invariants::InvariantConfig;

    use super::*;

    const DEMO_STARTUP: ServiceKey<DemoStartup> = ServiceKey::new("demoStartup");

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct DemoStartup {
        port: Option<u16>,
    }

    #[derive(Default)]
    struct RecordingOutput {
        stdout: Mutex<String>,
        stderr: Mutex<String>,
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
        action: bool,
        fatal: bool,
    }

    impl CmdlineProgram for DemoProgram {
        type Action = DemoStartup;

        fn name(&self) -> &'static str {
            "demo"
        }

        fn has_action(&self) -> bool {
            self.action
        }

        fn parse(
            &mut self,
            args: &[String],
        ) -> anyhow::Result<CmdlineProgramOutcome<Self::Action>> {
            if self.fatal {
                anyhow::bail!("action exploded");
            }
            match args {
                [flag] if flag == "--help" => Ok(CmdlineProgramOutcome::Exit {
                    code: 0,
                    stdout: "Usage: demo\n".to_owned(),
                    stderr: String::new(),
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
                [] => Ok(CmdlineProgramOutcome::Action(DemoStartup { port: None })),
                _ => anyhow::bail!("unexpected demo argv: {args:?}"),
            }
        }

        fn run_action(&mut self, context: &Context, action: Self::Action) -> anyhow::Result<()> {
            context.provide(DEMO_STARTUP, Arc::new(action))?;
            Ok(())
        }
    }

    fn host(context: &Context, args: &[&str], exits: Arc<Mutex<Vec<i32>>>) -> anyhow::Result<()> {
        provide_cmdline(
            context,
            CmdlineHost::new(args.iter().copied(), move |code| {
                exits.lock().push(code);
                Ok(())
            }),
        )
    }

    #[test]
    fn host_snapshot_is_immutable_and_shared_by_multiple_parsers() {
        let context = Context::new();
        let mut args = vec!["--port".to_owned(), "8080".to_owned()];
        let exits = Arc::new(Mutex::new(Vec::new()));
        provide_cmdline(
            &context,
            CmdlineHost::new(args.clone(), {
                let exits = exits.clone();
                move |code| {
                    exits.lock().push(code);
                    Ok(())
                }
            }),
        )
        .unwrap();
        args.push("--tampered".to_owned());
        assert_eq!(context.get(CMDLINE_ARGS).unwrap().get(), ["--port", "8080"]);

        let output = RecordingOutput::default();
        for _ in 0..2 {
            let mut program = DemoProgram {
                action: true,
                fatal: false,
            };
            parse_cmdline_with_output(&context.isolate(DEMO_STARTUP), &mut program, &output)
                .unwrap();
        }
        assert!(exits.lock().is_empty());
    }

    #[tokio::test]
    async fn valid_help_usage_and_fatal_paths_preserve_publication_and_exit_rules() {
        let valid = Context::new();
        let exits = Arc::new(Mutex::new(Vec::new()));
        host(&valid, &["--port", "8080"], exits.clone()).unwrap();
        let output = RecordingOutput::default();
        parse_cmdline_with_output(
            &valid,
            &mut DemoProgram {
                action: true,
                fatal: false,
            },
            &output,
        )
        .unwrap();
        assert_eq!(valid.get(DEMO_STARTUP).unwrap().port, Some(8080));
        assert!(exits.lock().is_empty());

        for (args, code, expected) in [
            (vec!["--help"], 0, "Usage: demo"),
            (vec!["--port", "abc"], 1, "--port must be a number"),
        ] {
            let context = Context::new();
            let exits = Arc::new(Mutex::new(Vec::new()));
            host(&context, &args, exits.clone()).unwrap();
            let output = RecordingOutput::default();
            parse_cmdline_with_output(
                &context,
                &mut DemoProgram {
                    action: true,
                    fatal: false,
                },
                &output,
            )
            .unwrap();
            assert_eq!(&*exits.lock(), &[code]);
            assert!(context.get(DEMO_STARTUP).is_none());
            let combined = format!("{}{}", output.stdout.lock(), output.stderr.lock());
            assert!(combined.contains(expected));
        }

        let fatal = Context::new();
        host(&fatal, &[], Arc::new(Mutex::new(Vec::new()))).unwrap();
        let error = parse_cmdline_with_output(
            &fatal,
            &mut DemoProgram {
                action: true,
                fatal: true,
            },
            &RecordingOutput::default(),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "action exploded");
        assert!(fatal.get(DEMO_STARTUP).is_none());
        fatal.fiber().dispose().await.unwrap();
        assert!(fatal.get(CMDLINE_ARGS).is_none());
        assert!(fatal.get(APP_EXIT).is_none());
    }

    #[test]
    fn missing_launcher_values_and_missing_actions_fail_loud() {
        let context = Context::new();
        let output = RecordingOutput::default();
        let missing = parse_cmdline_with_output(
            &context,
            &mut DemoProgram {
                action: true,
                fatal: false,
            },
            &output,
        )
        .unwrap_err();
        assert!(
            missing
                .to_string()
                .contains("ctx.cmdlineArgs and ctx.appExit")
        );

        host(&context, &[], Arc::new(Mutex::new(Vec::new()))).unwrap();
        let no_action = parse_cmdline_with_output(
            &context,
            &mut DemoProgram {
                action: false,
                fatal: false,
            },
            &output,
        )
        .unwrap_err();
        assert!(
            no_action
                .to_string()
                .contains("no command in the program declares an action")
        );
    }

    #[tokio::test]
    async fn invariant_companion_reserves_the_exact_package_and_unwinds() {
        assert_eq!(INVARIANT_PLUGIN_NAME, "cmdline-invariant");
        assert_eq!(INVARIANT_INJECT, ["invariants"]);
        let context = Context::new();
        let registry = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
        let registration = register_invariant(&registry).unwrap();
        registration.await_ready().await.unwrap();
        assert!(registry.is_registered(INVARIANT_PACKAGE));
        registration.dispose().await.unwrap();
        assert!(!registry.is_registered(INVARIANT_PACKAGE));
    }
}
