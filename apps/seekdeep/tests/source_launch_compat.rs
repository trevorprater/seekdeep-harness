//! Built-binary compatibility checks for source-owned launcher exits.

use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

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
    let mut command = Command::new(env!("CARGO_BIN_EXE_seekdeep"));
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    process_result(command.output().expect("seekdeep binary must launch"))
}

fn process_result(output: Output) -> ProcessResult {
    ProcessResult {
        code: output.status.code(),
        stdout: String::from_utf8(output.stdout).expect("seekdeep stdout must be UTF-8"),
        stderr: String::from_utf8(output.stderr).expect("seekdeep stderr must be UTF-8"),
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
fn headless_help_and_missing_task_exit_before_profile_boot() {
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
