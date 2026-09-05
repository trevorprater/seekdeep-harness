//! Rust entrypoints for the source-checkout Code Mode and Cordis demos.

use std::{
    ffi::{OsStr, OsString},
    io,
    process::{Command, Stdio},
};

const CODE_MODE_USAGE: &str = "usage: pnpm run demo:code-mode";
const CORDIS_USAGE: &str = "usage: pnpm run demo:cordis [web|acp]";
const CORDIS_WEB_ANNOUNCEMENT: &str = "Cordis Web: http://127.0.0.1:3081";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Demo {
    CodeMode,
    Cordis,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LaunchPlan {
    package: &'static str,
    application_arguments: &'static [&'static str],
    announcement: Option<&'static str>,
}

impl LaunchPlan {
    fn cargo_arguments(&self) -> Vec<OsString> {
        ["run", "--quiet", "--package", self.package, "--"]
            .into_iter()
            .chain(self.application_arguments.iter().copied())
            .map(OsString::from)
            .collect()
    }
}

fn code_mode_plan(arguments: &[OsString]) -> Result<LaunchPlan, &'static str> {
    if !arguments.is_empty() {
        return Err(CODE_MODE_USAGE);
    }
    Ok(LaunchPlan {
        package: "seekdeep-acp-demo",
        application_arguments: &["--config", "examples/acp-agent/code-mode.cordis.yml"],
        announcement: None,
    })
}

fn cordis_plan(arguments: &[OsString]) -> Result<LaunchPlan, &'static str> {
    let surface = match arguments {
        [] => "web",
        [surface] => surface.to_str().ok_or(CORDIS_USAGE)?,
        _ => return Err(CORDIS_USAGE),
    };
    match surface {
        "web" => Ok(LaunchPlan {
            package: "seekdeep",
            application_arguments: &["web", "--patch", "examples/web-cordis/cordis.yml"],
            announcement: Some(CORDIS_WEB_ANNOUNCEMENT),
        }),
        "acp" => Ok(LaunchPlan {
            package: "seekdeep-acp-demo",
            application_arguments: &["--config", "examples/acp-agent/cordis-tools.cordis.yml"],
            announcement: None,
        }),
        _ => Err(CORDIS_USAGE),
    }
}

fn run_with<W, E, F>(
    demo: Demo,
    arguments: &[OsString],
    cargo: &OsStr,
    stdout: &mut W,
    stderr: &mut E,
    mut launch: F,
) -> i32
where
    W: io::Write,
    E: io::Write,
    F: FnMut(&OsStr, &[OsString]) -> io::Result<Option<i32>>,
{
    let plan = match demo {
        Demo::CodeMode => code_mode_plan(arguments),
        Demo::Cordis => cordis_plan(arguments),
    };
    let plan = match plan {
        Ok(plan) => plan,
        Err(usage) => {
            let _ = writeln!(stderr, "{usage}");
            return 2;
        }
    };
    if let Some(announcement) = plan.announcement
        && (writeln!(stdout, "{announcement}").is_err() || stdout.flush().is_err())
    {
        return 1;
    }
    match launch(cargo, &plan.cargo_arguments()) {
        Ok(code) => code.unwrap_or(1),
        Err(error) => {
            let _ = writeln!(stderr, "seekdeep demo: failed to launch Cargo: {error}");
            1
        }
    }
}

fn process_main(demo: Demo) -> i32 {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr().lock();
    run_with(
        demo,
        &arguments,
        &cargo,
        &mut stdout,
        &mut stderr,
        |program, arguments| {
            let status = Command::new(program)
                .args(arguments)
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status()?;
            Ok(status.code())
        },
    )
}

/// Runs the Code Mode repository demo and returns the source-compatible exit code.
#[must_use]
pub fn code_mode_process_main() -> i32 {
    process_main(Demo::CodeMode)
}

/// Runs the Cordis repository demo and returns the source-compatible exit code.
#[must_use]
pub fn cordis_process_main() -> i32 {
    process_main(Demo::Cordis)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn code_mode_accepts_no_arguments_and_selects_the_compiled_acp_app() {
        let plan = code_mode_plan(&[]).unwrap();
        assert_eq!(
            plan.cargo_arguments(),
            strings(&[
                "run",
                "--quiet",
                "--package",
                "seekdeep-acp-demo",
                "--",
                "--config",
                "examples/acp-agent/code-mode.cordis.yml",
            ])
        );
        assert_eq!(plan.announcement, None);
        assert_eq!(code_mode_plan(&strings(&["extra"])), Err(CODE_MODE_USAGE));
    }

    #[test]
    fn cordis_defaults_to_web_and_accepts_only_the_two_source_surfaces() {
        let default = cordis_plan(&[]).unwrap();
        assert_eq!(default, cordis_plan(&strings(&["web"])).unwrap());
        assert_eq!(
            default.cargo_arguments(),
            strings(&[
                "run",
                "--quiet",
                "--package",
                "seekdeep",
                "--",
                "web",
                "--patch",
                "examples/web-cordis/cordis.yml",
            ])
        );
        assert_eq!(default.announcement, Some(CORDIS_WEB_ANNOUNCEMENT));
        assert_eq!(
            cordis_plan(&strings(&["acp"])).unwrap().cargo_arguments(),
            strings(&[
                "run",
                "--quiet",
                "--package",
                "seekdeep-acp-demo",
                "--",
                "--config",
                "examples/acp-agent/cordis-tools.cordis.yml",
            ])
        );
        for invalid in [&["nope"][..], &["web", "extra"][..]] {
            assert_eq!(cordis_plan(&strings(invalid)), Err(CORDIS_USAGE));
        }
    }

    #[test]
    fn process_contract_preserves_usage_output_announcements_and_child_statuses() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run_with(
            Demo::CodeMode,
            &strings(&["extra"]),
            OsStr::new("cargo"),
            &mut stdout,
            &mut stderr,
            |_, _| panic!("invalid input must not start a child"),
        );
        assert_eq!(code, 2);
        assert!(stdout.is_empty());
        assert_eq!(stderr, b"usage: pnpm run demo:code-mode\n");

        stdout.clear();
        stderr.clear();
        let mut observed = None;
        let code = run_with(
            Demo::Cordis,
            &[],
            OsStr::new("custom-cargo"),
            &mut stdout,
            &mut stderr,
            |program, arguments| {
                observed = Some((program.to_owned(), arguments.to_vec()));
                Ok(Some(17))
            },
        );
        assert_eq!(code, 17);
        assert_eq!(stdout, b"Cordis Web: http://127.0.0.1:3081\n");
        assert!(stderr.is_empty());
        assert_eq!(observed.unwrap().0, OsString::from("custom-cargo"));

        stdout.clear();
        assert_eq!(
            run_with(
                Demo::Cordis,
                &strings(&["acp"]),
                OsStr::new("cargo"),
                &mut stdout,
                &mut stderr,
                |_, _| Ok(None),
            ),
            1
        );
        assert!(stdout.is_empty());
    }

    #[test]
    fn child_launch_failures_are_loud_and_nonzero() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run_with(
            Demo::Cordis,
            &strings(&["acp"]),
            OsStr::new("cargo"),
            &mut stdout,
            &mut stderr,
            |_, _| Err(io::Error::new(io::ErrorKind::NotFound, "missing")),
        );
        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        assert_eq!(stderr, b"seekdeep demo: failed to launch Cargo: missing\n");
    }
}
