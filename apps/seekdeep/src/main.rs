//! `SeekDeep` Harness command-line entry point.

use std::{
    ffi::OsString,
    io::{self, Write},
    process::ExitCode,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicI32, Ordering},
    },
};

use seekdeep::{
    HeadlessApplication, HeadlessBootOptions,
    args::{
        DumpConfigInvocation, LauncherExit, ParseOutcome, PluginInvocation, ProfileInvocation,
        SeekDeepInvocation, launcher_help, parse_seekdeep_args,
    },
    process_shutdown::ProcessShutdown,
};
use seekdeep_headless::startup::{
    HeadlessStartupAction, HeadlessStartupValues, parse_headless_args,
};
use seekdeep_util::abort::AbortSignal;

const EXIT_CODE_UNSET: i32 = i32::MIN;

fn main() -> ExitCode {
    let argv = std::env::args_os().skip(1).collect::<Vec<_>>();
    ExitCode::from(normalize_exit_code(dispatch(&argv)))
}

fn dispatch(argv: &[OsString]) -> i32 {
    let argv = argv
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let outcome = match parse_seekdeep_args(&argv, env!("CARGO_PKG_VERSION")) {
        Ok(outcome) => outcome,
        Err(error) => {
            write_stderr(&format!("{error}\n"));
            return error.exit_code();
        }
    };

    match outcome {
        ParseOutcome::Exit(LauncherExit::Help) => {
            write_stdout(&launcher_help());
            0
        }
        ParseOutcome::Exit(LauncherExit::Version(version)) => {
            write_stdout(&format!("{version}\n"));
            0
        }
        ParseOutcome::Invocation(invocation) => dispatch_invocation(invocation),
    }
}

fn dispatch_invocation(invocation: SeekDeepInvocation) -> i32 {
    match invocation {
        SeekDeepInvocation::Profile(invocation) => dispatch_profile(invocation),
        SeekDeepInvocation::DumpConfig(invocation) => dump_not_yet_available(&invocation),
        SeekDeepInvocation::Plugin(invocation) => plugin_not_yet_available(&invocation),
    }
}

fn dispatch_profile(invocation: ProfileInvocation) -> i32 {
    if !invocation.patches.is_empty() {
        write_stderr(
            "seekdeep: profile overlays are not available until the Rust profile loader is complete\n",
        );
        return 1;
    }
    if invocation.profile.as_str() != "headless" {
        write_stderr(&format!(
            "seekdeep: profile {:?} is not available in the current Rust launcher\n",
            invocation.profile.as_str()
        ));
        return 1;
    }

    match parse_headless_args(&invocation.args) {
        HeadlessStartupAction::Exit {
            code,
            stdout,
            stderr,
        } => {
            write_stdout(&stdout);
            write_stderr(&stderr);
            code
        }
        HeadlessStartupAction::Run(HeadlessStartupValues { task }) => run_headless(task),
    }
}

fn dump_not_yet_available(invocation: &DumpConfigInvocation) -> i32 {
    let mode = if invocation.default_only {
        "--dump-default-config"
    } else {
        "--dump-config"
    };
    write_stderr(&format!(
        "seekdeep: {mode} for profile {:?} is not available until Rust profile composition is complete\n",
        invocation.profile.as_str()
    ));
    1
}

fn plugin_not_yet_available(invocation: &PluginInvocation) -> i32 {
    write_stderr(&format!(
        "seekdeep: plugin management for profile {:?} is not available in the current Rust launcher\n",
        invocation.profile.as_str()
    ));
    1
}

fn run_headless(task: String) -> i32 {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            write_stderr(&format!(
                "seekdeep: failed to start async runtime: {error}\n"
            ));
            return 1;
        }
    };
    let result = runtime.block_on(run_headless_async(task));
    runtime.shutdown_background();
    match result {
        Ok(code) => code,
        Err(error) => {
            write_stderr(&format!("seekdeep: {error}\n"));
            1
        }
    }
}

async fn run_headless_async(task: String) -> anyhow::Result<i32> {
    let signals = SignalSources::prepare()?;
    let options = HeadlessBootOptions::from_process()?;
    let application_slot = Arc::new(ApplicationSlot::default());
    let startup_abort = AbortSignal::default();
    let completed_code = Arc::new(AtomicI32::new(EXIT_CODE_UNSET));
    let application_for_shutdown = Arc::clone(&application_slot);
    let completion_for_shutdown = Arc::clone(&completed_code);
    let shutdown = ProcessShutdown::new(
        move || async move { application_for_shutdown.shutdown().await },
        std::process::exit,
        move |code| completion_for_shutdown.store(code, Ordering::Release),
    );
    let signal_tasks = signals.spawn(shutdown.clone(), startup_abort.clone());

    let application =
        match HeadlessApplication::boot_with_abort(options, startup_abort.clone()).await {
            Ok(application) => {
                let application = Arc::new(application);
                application_slot.publish(Arc::clone(&application))?;
                application
            }
            Err(error) => {
                application_slot.finish_without_application();
                if startup_abort.is_aborted() {
                    shutdown.shutdown(1).await;
                }
                abort_tasks(signal_tasks);
                return Err(error);
            }
        };

    if startup_abort.is_aborted() {
        shutdown.shutdown(1).await;
    }

    let result = application.run(&task).await;
    let output_result = write_run_output(&result.stdout, &result.stderr);
    shutdown.shutdown(result.exit_code).await;
    abort_tasks(signal_tasks);
    output_result?;

    let completed_code = completed_code.load(Ordering::Acquire);
    anyhow::ensure!(
        completed_code != EXIT_CODE_UNSET,
        "application disposal ended without selecting an exit code"
    );
    Ok(completed_code)
}

#[derive(Debug, Default)]
struct ApplicationSlot {
    application: OnceLock<Arc<HeadlessApplication>>,
    boot_finished: AtomicBool,
    changed: tokio::sync::Notify,
}

impl ApplicationSlot {
    fn publish(&self, application: Arc<HeadlessApplication>) -> anyhow::Result<()> {
        self.application
            .set(application)
            .map_err(|_| anyhow::anyhow!("headless application was published more than once"))?;
        self.boot_finished.store(true, Ordering::Release);
        self.changed.notify_waiters();
        Ok(())
    }

    fn finish_without_application(&self) {
        self.boot_finished.store(true, Ordering::Release);
        self.changed.notify_waiters();
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        loop {
            let changed = self.changed.notified();
            if let Some(application) = self.application.get() {
                return application.shutdown().await;
            }
            if self.boot_finished.load(Ordering::Acquire) {
                return Ok(());
            }
            changed.await;
        }
    }
}

fn abort_tasks(tasks: Vec<tokio::task::JoinHandle<()>>) {
    for task in tasks {
        task.abort();
    }
}

fn write_run_output(stdout: &str, stderr: &str) -> io::Result<()> {
    io::stdout().lock().write_all(stdout.as_bytes())?;
    io::stderr().lock().write_all(stderr.as_bytes())
}

fn write_stdout(output: &str) {
    let _ = io::stdout().lock().write_all(output.as_bytes());
}

fn write_stderr(output: &str) {
    let _ = io::stderr().lock().write_all(output.as_bytes());
}

fn normalize_exit_code(code: i32) -> u8 {
    u8::try_from(code).unwrap_or(1)
}

#[cfg(unix)]
struct SignalSources {
    sigterm: tokio::signal::unix::Signal,
    sigint: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl SignalSources {
    fn prepare() -> io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};

        Ok(Self {
            sigterm: signal(SignalKind::terminate())?,
            sigint: signal(SignalKind::interrupt())?,
        })
    }

    fn spawn(
        self,
        shutdown: ProcessShutdown,
        startup_abort: AbortSignal,
    ) -> Vec<tokio::task::JoinHandle<()>> {
        let Self {
            mut sigterm,
            mut sigint,
        } = self;
        let shutdown_for_sigterm = shutdown.clone();
        let abort_for_sigterm = startup_abort.clone();
        vec![
            tokio::spawn(async move {
                while sigterm.recv().await.is_some() {
                    abort_for_sigterm.abort();
                    shutdown_for_sigterm.interrupt_sigterm();
                }
            }),
            tokio::spawn(async move {
                while sigint.recv().await.is_some() {
                    startup_abort.abort();
                    shutdown.interrupt_sigint();
                }
            }),
        ]
    }
}

#[cfg(not(unix))]
struct SignalSources;

#[cfg(not(unix))]
impl SignalSources {
    fn prepare() -> io::Result<Self> {
        Ok(Self)
    }

    fn spawn(
        self,
        shutdown: ProcessShutdown,
        startup_abort: AbortSignal,
    ) -> Vec<tokio::task::JoinHandle<()>> {
        vec![tokio::spawn(async move {
            while tokio::signal::ctrl_c().await.is_ok() {
                startup_abort.abort();
                shutdown.interrupt_sigint();
            }
        })]
    }
}
