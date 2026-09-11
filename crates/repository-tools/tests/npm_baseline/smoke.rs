use std::{collections::BTreeMap, ffi::OsString, path::PathBuf, time::Duration};

use seekdeep_repository_tools::npm_baseline::{
    InstalledWebProbe, installed_artifact_environment, npm_client_environment, probe_installed_web,
};

#[test]
fn installed_environment_excludes_workspace_injection_and_uses_isolated_homes() {
    let parent = BTreeMap::<OsString, OsString>::from([
        ("npm_config_user_agent".into(), "pnpm".into()),
        ("NPM_CONFIG_USER_AGENT".into(), "pnpm".into()),
        ("NODE_OPTIONS".into(), "--require untrusted".into()),
        ("NODE_PATH".into(), "/workspace".into()),
        ("COLORTERM".into(), "truecolor".into()),
        ("SOME_ALLOWED_VALUE".into(), "preserved".into()),
    ]);
    let consumer = PathBuf::from("/tmp/isolated consumer");
    let npm = npm_client_environment(&parent);
    assert!(!npm.contains_key(std::ffi::OsStr::new("npm_config_user_agent")));
    assert!(!npm.contains_key(std::ffi::OsStr::new("NPM_CONFIG_USER_AGENT")));
    assert!(npm.contains_key(std::ffi::OsStr::new("NODE_OPTIONS")));
    let installed = installed_artifact_environment(&parent, &consumer);
    for key in [
        "NODE_OPTIONS",
        "NODE_PATH",
        "COLORTERM",
        "npm_config_user_agent",
        "NPM_CONFIG_USER_AGENT",
    ] {
        assert!(!installed.contains_key(std::ffi::OsStr::new(key)));
    }
    assert_eq!(
        installed[std::ffi::OsStr::new("SEEKDEEP_HOME")],
        consumer.join(".seekdeep")
    );
    assert_eq!(
        installed[std::ffi::OsStr::new("SEEKDEEP_AGENTS_HOME")],
        consumer.join(".agents")
    );
    assert_eq!(
        installed[std::ffi::OsStr::new("DEEPSEEK_API_KEY")],
        "keyless-installed-web-no-call"
    );
    assert_eq!(
        installed[std::ffi::OsStr::new("SOME_ALLOWED_VALUE")],
        "preserved"
    );
    assert_eq!(parent[std::ffi::OsStr::new("NODE_PATH")], "/workspace");
}

#[cfg(unix)]
#[test]
fn native_pty_probe_requires_readiness_clean_shutdown_and_reaps_timed_out_children() {
    use nix::{errno::Errno, sys::signal::kill, unistd::Pid};
    for (behavior, expected) in [
        ("ready", None),
        ("early", Some("did not reach its ready URL")),
        ("nonzero", Some("exited 7, expected 0")),
        ("timeout", Some("did not reach its ready URL")),
        ("ignore", Some("exited -9, expected 0")),
    ] {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("entry.mjs");
        std::fs::write(
            &bin,
            format!(
                r"
import fs from 'node:fs';
fs.writeFileSync('pid', String(process.pid));
if (!process.stdin.isTTY || !process.stdout.isTTY) throw Error('missing PTY');
const behavior = '{behavior}';
if (behavior === 'early') process.exit(0);
setInterval(() => {{}}, 1000);
if (behavior === 'ready') process.on('SIGTERM', () => process.exit(0));
if (behavior === 'nonzero') process.on('SIGTERM', () => process.exit(7));
if (behavior === 'ignore') process.on('SIGTERM', () => {{}});
if (behavior !== 'timeout') process.stdout.write('seekdeep web: http://127.0.0.1:9876\n');
"
            ),
        )
        .unwrap();
        let probe = InstalledWebProbe {
            node: "node".into(),
            bin,
            cwd: root.path().to_owned(),
            environment: installed_artifact_environment(
                &super::support::environment(),
                root.path(),
            ),
            timeout: Duration::from_millis(800),
        };
        let result = probe_installed_web(&probe);
        if let Some(expected) = expected {
            assert!(
                result.unwrap_err().to_string().contains(expected),
                "{behavior}"
            );
        } else {
            result.unwrap();
        }
        let pid = std::fs::read_to_string(root.path().join("pid"))
            .unwrap()
            .parse::<i32>()
            .unwrap();
        assert_eq!(
            kill(Pid::from_raw(pid), None),
            Err(Errno::ESRCH),
            "unreaped child for {behavior}"
        );
    }
}
