//! Built-binary compatibility checks for source-owned launcher exits.

use std::{
    fs,
    io::Read as _,
    path::Path,
    process::{Command, Output},
    sync::mpsc,
    time::Duration,
};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

const SOURCE_RENAMED_LAUNCHER_HELP: &str = r#"Usage: seekdeep [options] [command] [args...]

seekdeep: boot a SeekDeep Harness profile — an ordered stack of plugin-bundle
patch layers under your own overrides.

Arguments:
  args                        arguments for the booted profile's app (see:
                              seekdeep --profile <name> --help)

Options:
  -V, --version               output the version number
  --profile <name>            the profile under $SEEKDEEP_HOME/profiles to boot
  --patch <path>              extra patch-list overlay applied after the profile
                              layer (repeatable)
  --dump-config               print the composed profile tree and exit
  --dump-default-config       print the profile tree without its user layer or
                              --patch overlays and exit

Commands:
  web [options] [args...]     boot the web profile (alias of --profile web); the
                              web app's own flags follow
  plugin [options] [args...]  manage a profile's plugins by forwarding the
                              remaining arguments to pnpm in the profile
                              directory

Examples:
  seekdeep --profile web                          boot the web profile (same as: seekdeep web)
  seekdeep --profile headless "run the tests"     answer one task, print the result, and exit
  seekdeep --profile tui --patch ./extra.yml      boot a custom profile with one extra overlay
  seekdeep --profile tui --resume <session>       arguments after the launcher flags reach the app
  seekdeep --profile web --help                   the web app's own flags and help
  seekdeep plugin --profile tui add <package>     install a plugin into the tui profile

"#;

const SOURCE_RENAMED_HEADLESS_HELP: &str = concat!(
    "Usage: seekdeep --profile headless [options] [task...]\n\n",
    "Answer one task, print the final assistant message, and exit.\n\n",
    "Arguments:\n",
    "  task        the task text; multiple words are joined by spaces\n\n",
    "Options:\n",
    "  -h, --help  show this help\n\n",
    "Examples:\n",
    "  seekdeep --profile headless \"run the tests\"     answer one task and exit\n\n",
);

const MISSING_PROFILE_ERROR: &str = "error: --profile <name> is required\n";
const MISSING_HEADLESS_TASK_ERROR: &str = concat!(
    "error: a task is required, for example: seekdeep --profile headless ",
    "\"run the tests\"\n",
);

#[derive(Debug, Eq, PartialEq)]
struct ProcessResult {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn run_seekdeep(args: &[&str], cwd: Option<&Path>) -> ProcessResult {
    let sandbox = tempfile::tempdir().expect("temporary seekdeep process sandbox");
    let home = sandbox.path().join("home");
    let default_cwd = sandbox.path().join("workspace");
    fs::create_dir(&home).expect("temporary SEEKDEEP_HOME");
    fs::create_dir(&default_cwd).expect("temporary working directory");

    let mut command = Command::new(env!("CARGO_BIN_EXE_seekdeep"));
    command
        .args(args)
        .env_clear()
        .env("SEEKDEEP_HOME", &home)
        .current_dir(cwd.unwrap_or(&default_cwd));
    process_result(command.output().expect("seekdeep binary must launch"))
}

fn process_result(output: Output) -> ProcessResult {
    ProcessResult {
        code: output.status.code(),
        stdout: String::from_utf8(output.stdout).expect("seekdeep stdout must be UTF-8"),
        stderr: String::from_utf8(output.stderr).expect("seekdeep stderr must be UTF-8"),
    }
}

fn run_help_in_pty(columns: u16) -> ProcessResult {
    let sandbox = tempfile::tempdir().expect("temporary seekdeep PTY sandbox");
    let home = sandbox.path().join("home");
    let workspace = sandbox.path().join("workspace");
    fs::create_dir(&home).expect("temporary PTY SEEKDEEP_HOME");
    fs::create_dir(&workspace).expect("temporary PTY working directory");
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: columns,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("native PTY allocation");
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_seekdeep"));
    command.arg("--help");
    command.cwd(&workspace);
    command.env_clear();
    command.env("SEEKDEEP_HOME", &home);
    command.env("TERM", "dumb");
    let mut child = pair
        .slave
        .spawn_command(command)
        .expect("seekdeep PTY process must launch");
    let mut killer = child.clone_killer();
    let mut reader = pair.master.try_clone_reader().expect("seekdeep PTY reader");
    drop(pair.slave);
    drop(pair.master);

    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut output = Vec::new();
        let read = reader.read_to_end(&mut output);
        let status = child.wait();
        let _ = sender.send((read, status, output));
    });
    let (read, status, output) = match receiver.recv_timeout(Duration::from_secs(10)) {
        Ok(result) => result,
        Err(error) => {
            killer
                .kill()
                .expect("timed-out seekdeep PTY must be killed");
            let cleanup = receiver.recv_timeout(Duration::from_secs(5));
            panic!("seekdeep PTY help timed out: {error}; cleanup: {cleanup:#?}");
        }
    };
    read.expect("seekdeep PTY output must be readable");
    let status = status.expect("seekdeep PTY process must be waitable");
    ProcessResult {
        code: Some(i32::try_from(status.exit_code()).expect("PTY exit code fits i32")),
        stdout: String::from_utf8(output)
            .expect("seekdeep PTY stdout must be UTF-8")
            .replace("\r\n", "\n"),
        stderr: String::new(),
    }
}

#[test]
fn bare_invocation_requires_a_profile_without_booting() {
    assert_eq!(
        run_seekdeep(&[], None),
        ProcessResult {
            code: Some(1),
            stdout: String::new(),
            stderr: MISSING_PROFILE_ERROR.to_owned(),
        }
    );
}

#[test]
fn launcher_help_and_version_match_the_product_renamed_source_oracle() {
    assert_eq!(
        run_seekdeep(&["--help"], None),
        ProcessResult {
            code: Some(0),
            stdout: SOURCE_RENAMED_LAUNCHER_HELP.to_owned(),
            stderr: String::new(),
        }
    );
    assert_eq!(
        run_seekdeep(&["--version"], None),
        ProcessResult {
            code: Some(0),
            stdout: concat!(env!("CARGO_PKG_VERSION"), "\n").to_owned(),
            stderr: String::new(),
        }
    );
}

#[test]
fn launcher_help_uses_the_live_stdout_terminal_width() {
    let result = run_help_in_pty(60);
    assert_eq!(result.code, Some(0));
    assert_eq!(result.stderr, "");
    assert!(result.stdout.ends_with("\n\n"));
    assert!(result.stdout.contains(concat!(
        "seekdeep: boot a SeekDeep Harness profile — an ordered stack\n",
        "of plugin-bundle patch layers under your own overrides.\n",
    )));
    assert!(!result.stdout.contains(concat!(
        "seekdeep: boot a SeekDeep Harness profile — an ordered stack of plugin-bundle\n",
        "patch layers under your own overrides.\n",
    )));
}

#[test]
fn headless_help_and_missing_task_match_the_product_renamed_source_oracle() {
    assert_eq!(
        run_seekdeep(&["--profile", "headless", "--help"], None),
        ProcessResult {
            code: Some(0),
            stdout: SOURCE_RENAMED_HEADLESS_HELP.to_owned(),
            stderr: String::new(),
        }
    );
    assert_eq!(
        run_seekdeep(&["--profile", "headless"], None),
        ProcessResult {
            code: Some(1),
            stdout: String::new(),
            stderr: MISSING_HEADLESS_TASK_ERROR.to_owned(),
        }
    );
}

#[test]
fn launcher_help_and_version_do_not_read_the_invoking_directory_env_file() {
    let project = tempfile::tempdir().expect("temporary project directory");
    // A profile boot would try to read this layer and emit a diagnostic. These
    // launcher-owned exits must finish before environment composition begins.
    fs::create_dir(project.path().join(".env")).expect("hostile .env directory");

    assert_eq!(
        run_seekdeep(&["--help"], Some(project.path())),
        ProcessResult {
            code: Some(0),
            stdout: SOURCE_RENAMED_LAUNCHER_HELP.to_owned(),
            stderr: String::new(),
        }
    );
    assert_eq!(
        run_seekdeep(&["--version"], Some(project.path())),
        ProcessResult {
            code: Some(0),
            stdout: concat!(env!("CARGO_PKG_VERSION"), "\n").to_owned(),
            stderr: String::new(),
        }
    );
}
