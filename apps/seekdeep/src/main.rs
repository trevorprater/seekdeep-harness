//! `SeekDeep` Harness command-line entry point.

use std::{
    ffi::OsString,
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
};

use seekdeep::args::{
    DumpConfigInvocation, LauncherExit, ParseOutcome, PluginInvocation, ProfileInvocation,
    SeekDeepInvocation, launcher_help, parse_seekdeep_args,
};

fn main() -> ExitCode {
    if std::env::var_os("SEEKDEEP_INTERNAL_WORKFLOW_WORKER").as_deref()
        == Some(std::ffi::OsStr::new("1"))
    {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                write_stderr(&format!("seekdeep-workflow-worker: {error}\n"));
                return ExitCode::FAILURE;
            }
        };
        return match runtime.block_on(seekdeep_workflow_worker_thread::worker::run_stdio_worker()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                write_stderr(&format!("seekdeep-workflow-worker: {error:#}\n"));
                ExitCode::FAILURE
            }
        };
    }
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
        SeekDeepInvocation::Profile(invocation) => run_loader_profile(invocation),
        SeekDeepInvocation::DumpConfig(invocation) => dispatch_dump_config(&invocation),
        SeekDeepInvocation::Plugin(invocation) => dispatch_plugin(&invocation),
    }
}

fn run_loader_profile(invocation: ProfileInvocation) -> i32 {
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
    match runtime.block_on(run_loader_profile_async(invocation)) {
        Ok(code) => code,
        Err(error) => {
            write_stderr(&format!("{error:#}\n"));
            1
        }
    }
}

async fn run_loader_profile_async(invocation: ProfileInvocation) -> anyhow::Result<i32> {
    let cwd = std::env::current_dir()?;
    let layered = seekdeep::layered_env::load_layered_env("seekdeep", &cwd)?;
    let telemetry_disabled = layered
        .launch_environment
        .get(seekdeep::profile_boot::TELEMETRY_DISABLED_ENV)
        .map(|entry| entry.value);
    let plan = seekdeep::profile_boot::compose_profile_at(
        invocation.profile.as_str(),
        &invocation
            .patches
            .iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>(),
        &cwd,
        &layered.seekdeep_home,
        &seekdeep::profile_support::install_anchor(&layered.seekdeep_home),
        &seekdeep::profile_boot::shipped_preset_root(),
        telemetry_disabled.as_deref(),
    )?;
    let catalog = seekdeep::profile_boot::framework_profile_catalog(
        &cwd,
        &layered.seekdeep_home,
        &layered.launch_environment,
    )?;
    let running = seekdeep::profile_boot::run_profile_process(
        plan,
        &catalog,
        layered.launch_environment,
        invocation.args,
    )
    .await?;
    running.wait().await
}

fn dispatch_dump_config(invocation: &DumpConfigInvocation) -> i32 {
    match seekdeep::profile_support::dump_profile_config(invocation) {
        Ok(output) => {
            write_stdout(&output);
            0
        }
        Err(error) => {
            write_stderr(&format!("{error:#}\n"));
            1
        }
    }
}

fn dispatch_plugin(invocation: &PluginInvocation) -> i32 {
    match seekdeep::plugin_support::run_plugin(invocation) {
        Ok(code) => code,
        Err(error) => {
            write_stderr(&format!("{error:#}\n"));
            1
        }
    }
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
