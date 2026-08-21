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
use seekdeep_cmdline::CmdlineHost;
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

    run_headless(invocation.args)
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

fn run_headless(args: Vec<String>) -> i32 {
    let options = match HeadlessBootOptions::from_process() {
        Ok(options) => options,
        Err(error) => {
            write_headless_startup_error(&error);
            return 1;
        }
    };
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
    let result = runtime.block_on(run_headless_async(args, options));
    drop(runtime);
    match result {
        Ok(code) => code,
        Err(error) => {
            write_stderr(&format!("seekdeep: {error}\n"));
            1
        }
    }
}

async fn run_headless_async(
    args: Vec<String>,
    mut options: HeadlessBootOptions,
) -> anyhow::Result<i32> {
    let signals = SignalSources::prepare()?;
    let application_slot = Arc::new(ApplicationSlot::default());
    let startup_abort = AbortSignal::default();
    let completed_code = Arc::new(AtomicI32::new(EXIT_CODE_UNSET));
    let app_exit_code = Arc::new(AtomicI32::new(EXIT_CODE_UNSET));
    let application_for_shutdown = Arc::clone(&application_slot);
    let completion_for_shutdown = Arc::clone(&completed_code);
    let shutdown = ProcessShutdown::new(
        move || async move { application_for_shutdown.shutdown().await },
        std::process::exit,
        move |code| completion_for_shutdown.store(code, Ordering::Release),
    );
    let signal_tasks = signals.spawn(shutdown.clone(), startup_abort.clone());
    options = attach_cmdline_host(options, args, &shutdown, &startup_abort, &app_exit_code);

    let application =
        match HeadlessApplication::boot_with_abort(options, startup_abort.clone()).await {
            Ok(application) => {
                let application = Arc::new(application);
                if let Err(error) = application_slot.publish(Arc::clone(&application)) {
                    use std::fmt::Write as _;

                    application_slot.finish_without_application();
                    let application_cleanup = application.shutdown().await;
                    let signal_cleanup = stop_signal_tasks(signal_tasks).await;
                    let mut message = format!("{error:#}");
                    if let Err(cleanup) = application_cleanup {
                        write!(
                            message,
                            "\nunpublished application cleanup failed: {cleanup:#}"
                        )
                        .expect("writing to a String is infallible");
                    }
                    if let Err(cleanup) = signal_cleanup {
                        write!(message, "\nsignal-task cleanup failed: {cleanup:#}")
                            .expect("writing to a String is infallible");
                    }
                    return Err(anyhow::anyhow!(message));
                }
                application
            }
            Err(error) => {
                application_slot.finish_without_application();
                if startup_abort.is_aborted() {
                    shutdown.shutdown(1).await;
                }
                let signal_result = stop_signal_tasks(signal_tasks).await;
                if app_exit_code.load(Ordering::Acquire) != EXIT_CODE_UNSET {
                    signal_result?;
                    return require_completed_code(
                        completed_code.as_ref(),
                        "application exit disposal ended without selecting an exit code",
                    );
                }
                return match signal_result {
                    Ok(()) => Err(error),
                    Err(cleanup) => Err(anyhow::anyhow!(
                        "{error:#}\nsignal-task cleanup failed: {cleanup:#}"
                    )),
                };
            }
        };

    if startup_abort.is_aborted() {
        shutdown.shutdown(1).await;
        let signal_result = stop_signal_tasks(signal_tasks).await;
        if app_exit_code.load(Ordering::Acquire) != EXIT_CODE_UNSET {
            signal_result?;
            return require_completed_code(
                completed_code.as_ref(),
                "application exit disposal ended without selecting an exit code",
            );
        }
        anyhow::bail!("signal-driven shutdown returned without forcing process exit");
    }

    let result = application.run_startup().await;
    let (exit_code, stdout, stderr) = (result.exit_code, result.stdout, result.stderr);
    let output_result = write_run_output(&stdout, &stderr);
    shutdown.shutdown(exit_code).await;
    let signal_result = stop_signal_tasks(signal_tasks).await;
    match (output_result, signal_result) {
        (Err(output), Err(signals)) => {
            return Err(anyhow::anyhow!(
                "headless output failed: {output}\nsignal-task cleanup failed: {signals:#}"
            ));
        }
        (Err(output), Ok(())) => return Err(output.into()),
        (Ok(()), Err(signals)) => return Err(signals),
        (Ok(()), Ok(())) => {}
    }

    require_completed_code(
        completed_code.as_ref(),
        "application disposal ended without selecting an exit code",
    )
}

fn attach_cmdline_host(
    options: HeadlessBootOptions,
    args: Vec<String>,
    shutdown: &ProcessShutdown,
    startup_abort: &AbortSignal,
    app_exit_code: &Arc<AtomicI32>,
) -> HeadlessBootOptions {
    let shutdown = shutdown.clone();
    let startup_abort = startup_abort.clone();
    let app_exit_code = app_exit_code.clone();
    options.with_cmdline(CmdlineHost::new(args, move |code| {
        let _ = app_exit_code.compare_exchange(
            EXIT_CODE_UNSET,
            code,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        startup_abort.abort();
        drop(shutdown.shutdown(code));
        Ok(())
    }))
}

fn require_completed_code(value: &AtomicI32, missing: &str) -> anyhow::Result<i32> {
    let value = value.load(Ordering::Acquire);
    anyhow::ensure!(value != EXIT_CODE_UNSET, "{missing}");
    Ok(value)
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

async fn stop_signal_tasks(tasks: Vec<tokio::task::JoinHandle<()>>) -> anyhow::Result<()> {
    for task in &tasks {
        task.abort();
    }
    let mut errors = Vec::new();
    for task in tasks {
        match task.await {
            Ok(()) => {}
            Err(error) if error.is_cancelled() => {}
            Err(error) => errors.push(error.to_string()),
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(errors.join("\n")))
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

fn write_headless_startup_error(error: &anyhow::Error) {
    if matches!(
        error.downcast_ref::<seekdeep::layered_env::LoadLayeredEnvError>(),
        Some(seekdeep::layered_env::LoadLayeredEnvError::BootstrapOnly { .. })
    ) {
        write_stderr(&format!("{error}\n"));
    } else {
        write_stderr(&format!("seekdeep: {error}\n"));
    }
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

#[cfg(windows)]
struct SignalSources {
    ctrl_c: tokio::signal::windows::CtrlC,
}

#[cfg(windows)]
impl SignalSources {
    fn prepare() -> io::Result<Self> {
        Ok(Self {
            ctrl_c: tokio::signal::windows::ctrl_c()?,
        })
    }

    fn spawn(
        mut self,
        shutdown: ProcessShutdown,
        startup_abort: AbortSignal,
    ) -> Vec<tokio::task::JoinHandle<()>> {
        vec![tokio::spawn(async move {
            while self.ctrl_c.recv().await.is_some() {
                startup_abort.abort();
                shutdown.interrupt_sigint();
            }
        })]
    }
}

#[cfg(not(any(unix, windows)))]
struct SignalSources;

#[cfg(not(any(unix, windows)))]
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
