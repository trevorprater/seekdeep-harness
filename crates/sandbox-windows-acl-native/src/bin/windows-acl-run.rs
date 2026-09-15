//! Compiled Rust confinement runner preserving the stable source argv contract.

#[cfg(windows)]
use std::sync::Arc;

#[cfg(windows)]
use seekdeep_sandbox_windows_acl::{
    AclSandbox, AclSandboxMode, AclSandboxOptions, RUNNER_FAILURE_EXIT, RUNNER_SIGNATURE,
    SandboxStdio, WindowsAclBindings, parse_runner_args, temp_write_sid, validate_runner_args,
    workspace_write_sid,
};
#[cfg(windows)]
use seekdeep_sandbox_windows_acl_native::WindowsBindings;

#[cfg(windows)]
fn fatal(detail: impl std::fmt::Display) -> i32 {
    eprintln!("{RUNNER_SIGNATURE}: {detail}");
    RUNNER_FAILURE_EXIT
}

#[cfg(windows)]
async fn run() -> Result<u32, String> {
    let raw = std::env::args().skip(1).collect::<Vec<_>>();
    let parsed = parse_runner_args(&raw).map_err(|error| error.to_string())?;
    validate_runner_args(&parsed).map_err(|error| error.to_string())?;

    let native = WindowsBindings;
    if !native.ignore_ctrl_c() {
        return Err(format!(
            "SetConsoleCtrlHandler failed (Win32 {})",
            seekdeep_sandbox_windows_acl::AclBindings::last_error(&native)
        ));
    }

    let seam_managed = parsed.write_sid.is_some() || parsed.temp_write_sid.is_some();
    let mut owned_temp = None;
    let mut sandbox = None;
    let mut initialized = false;
    let attempt = async {
        let (private_temp, write_sid, private_temp_sid) = match parsed.mode {
            AclSandboxMode::ReadOnly => (None, None, None),
            AclSandboxMode::WorkspaceWrite => {
                let workspace = parsed
                    .workspace
                    .to_str()
                    .ok_or_else(|| "runner path is not valid Unicode".to_owned())?;
                let write_sid = workspace_write_sid(workspace);
                if seam_managed {
                    let private = parsed.temp.clone();
                    let private_text = private
                        .to_str()
                        .ok_or_else(|| "runner path is not valid Unicode".to_owned())?;
                    let private_sid = temp_write_sid(private_text);
                    (Some(private), Some(write_sid), Some(private_sid))
                } else {
                    let directory = tempfile::Builder::new()
                        .prefix("seekdeep-")
                        .tempdir_in(&parsed.temp)
                        .map_err(|error| error.to_string())?
                        .keep();
                    let directory_text = directory
                        .to_str()
                        .ok_or_else(|| "runner path is not valid Unicode".to_owned())?;
                    let temp_sid = temp_write_sid(directory_text);
                    owned_temp = Some(directory.clone());
                    (Some(directory), Some(write_sid), Some(temp_sid))
                }
            }
        };

        let binding: Arc<dyn WindowsAclBindings> = Arc::new(native);
        let options = AclSandboxOptions {
            writable_dirs: if parsed.mode == AclSandboxMode::WorkspaceWrite {
                vec![parsed.workspace.clone()]
            } else {
                Vec::new()
            },
            temp_dir: private_temp.clone(),
            temp_was_explicit: true,
            write_sid,
            temp_write_sid: private_temp_sid,
            mode: parsed.mode,
            manage_dacls: !seam_managed,
        };
        let mut owner = AclSandbox::new(&options, binding).map_err(|error| error.to_string())?;
        owner
            .init(std::process::id())
            .map_err(|error| error.to_string())?;
        initialized = true;
        sandbox = Some(owner);

        if let Some(private_temp) = &private_temp {
            if !native.set_environment_variable("TMP", private_temp) {
                return Err(format!(
                    "SetEnvironmentVariableW TMP failed (Win32 {})",
                    seekdeep_sandbox_windows_acl::AclBindings::last_error(&native)
                ));
            }
            if !native.set_environment_variable("TEMP", private_temp) {
                return Err(format!(
                    "SetEnvironmentVariableW TEMP failed (Win32 {})",
                    seekdeep_sandbox_windows_acl::AclBindings::last_error(&native)
                ));
            }
        }

        let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
        let owner = sandbox
            .as_ref()
            .ok_or_else(|| "AclSandbox was not retained after init".to_owned())?;
        let child = owner
            .spawn(
                &parsed.command[0],
                &parsed.command[1..],
                &cwd,
                SandboxStdio::Inherit,
            )
            .map_err(|error| error.to_string())?;
        child
            .wait()
            .await
            .map(|result| result.exit_code)
            .map_err(|error| error.to_string())
    }
    .await;

    if initialized
        && let Some(mut owner) = sandbox
        && let Err(error) = owner.dispose()
    {
        eprintln!("{RUNNER_SIGNATURE}: cleanup: {error}");
    }
    if let Some(directory) = owned_temp {
        match std::fs::remove_dir_all(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => eprintln!("{RUNNER_SIGNATURE}: cleanup: {error}"),
        }
    }
    attempt
}

#[cfg(windows)]
#[tokio::main]
async fn main() {
    let native = WindowsBindings;
    match run().await {
        Ok(code) => native.exit_process(code),
        Err(error) => native.exit_process(fatal(error).cast_unsigned()),
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("windows-acl-run: native Windows ACL confinement is unavailable on this platform");
    std::process::exit(127);
}
