//! Environment-boundary parity for the E2B subprocess provider.

use std::{collections::BTreeMap, sync::Arc};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use parking_lot::Mutex;
use seekdeep_e2b::{E2bCommandResult, E2bCommands};
use seekdeep_llm::AbortSignal;
use seekdeep_subprocess::SubprocessEnvironment;
use seekdeep_subprocess_e2b::environment::{
    bootstrap_environment, read_remote_environment, scrub_remote_environment,
    serialize_remote_environment,
};

type CommandRequest = (String, BTreeMap<String, String>, bool);

#[derive(Debug)]
struct FakeCommands {
    output: Mutex<E2bCommandResult>,
    requests: Mutex<Vec<CommandRequest>>,
}

impl FakeCommands {
    fn new(stdout: String) -> Arc<Self> {
        Arc::new(Self {
            output: Mutex::new(E2bCommandResult {
                stdout,
                stderr: String::new(),
            }),
            requests: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait::async_trait]
impl E2bCommands for FakeCommands {
    async fn run(
        &self,
        command: &str,
        env: BTreeMap<String, String>,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<E2bCommandResult> {
        self.requests
            .lock()
            .push((command.to_owned(), env, signal.is_some()));
        Ok(self.output.lock().clone())
    }
}

fn transport(home: &[u8], environment: &[u8]) -> String {
    format!(
        "{}\n{}",
        STANDARD.encode(home),
        STANDARD.encode(environment)
    )
}

#[tokio::test]
async fn reads_base64_environment_and_restores_the_passwd_login_home() {
    let commands = FakeCommands::new(transport(
        b"/home/e2b",
        "PATH=/bin\0HOME=/ambient\0UNICODE=你好\0".as_bytes(),
    ));
    let raw = read_remote_environment(commands.as_ref(), None)
        .await
        .expect("remote environment");
    assert_eq!(raw, "PATH=/bin\0HOME=/home/e2b\0UNICODE=你好\0");
    let requests = commands.requests.lock();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].0.contains("getent passwd \"$(id -u)\""));
    assert!(requests[0].0.contains("env -0 | base64 -w 0"));
    assert!(!requests[0].0.contains("$PWD"));
    assert!(requests[0].1["HOME"].starts_with("/.seekdeep-e2b-control-"));
}

#[tokio::test]
async fn rejects_invalid_framing_utf8_and_login_homes() {
    let cases = [
        ("missing second line".to_owned(), "invalid base64"),
        ("%%%\nAAAA".to_owned(), "invalid base64"),
        (transport(&[0xff], b"A=B\0"), "not valid UTF-8"),
        (
            transport(b"relative/home", b"A=B\0"),
            "login home is invalid",
        ),
        (transport(b"/home\0bad", b"A=B\0"), "login home is invalid"),
    ];
    for (wire, expected) in cases {
        let error = read_remote_environment(FakeCommands::new(wire).as_ref(), None)
            .await
            .unwrap_err();
        assert!(error.to_string().contains(expected), "{error:#}");
    }
}

#[test]
fn scrubs_ambient_secrets_but_explicit_values_remain_opted_in() {
    let raw = concat!(
        "PATH=/ambient/bin\0",
        "KEEP=safe\0",
        "UNICODE=你好\0",
        "NPM_TOKEN=secret\0",
        "PASSWORD_FILE=/secret\0",
        "SEEKDEEP_STALE=old\0",
        "BROKEN\0",
        "=bad\0",
    );
    assert_eq!(
        scrub_remote_environment(raw)
            .into_iter()
            .collect::<Vec<_>>(),
        [
            ("PATH".to_owned(), "/ambient/bin".to_owned()),
            ("KEEP".to_owned(), "safe".to_owned()),
            ("UNICODE".to_owned(), "你好".to_owned()),
        ]
    );
    let explicit = SubprocessEnvironment::from([
        ("KEEP".to_owned(), None),
        ("NPM_TOKEN".to_owned(), Some("explicit".to_owned())),
        ("SEEKDEEP_SESSION_ID".to_owned(), Some("session".to_owned())),
    ]);
    assert_eq!(
        serialize_remote_environment(raw, Some(&explicit)).expect("serialized"),
        "PATH=/ambient/bin\0UNICODE=你好\0NPM_TOKEN=explicit\0SEEKDEEP_SESSION_ID=session\0"
    );
}

#[test]
fn bootstrap_blanks_unsafe_names_and_environment_validation_fails_early() {
    let bootstrap =
        bootstrap_environment("PATH=/bin\0TERM=xterm\0API_KEY=secret\0seekdeep_private=value\0");
    assert_eq!(bootstrap["TERM"], "dumb");
    assert_eq!(bootstrap["API_KEY"], "");
    assert_eq!(bootstrap["seekdeep_private"], "");
    assert!(!bootstrap.contains_key("PATH"));

    for explicit in [
        SubprocessEnvironment::from([(String::new(), Some("x".to_owned()))]),
        SubprocessEnvironment::from([("BAD=NAME".to_owned(), Some("x".to_owned()))]),
        SubprocessEnvironment::from([("BAD".to_owned(), Some("x\0y".to_owned()))]),
    ] {
        assert!(
            serialize_remote_environment("PATH=/bin\0", Some(&explicit))
                .unwrap_err()
                .to_string()
                .contains("environment entries")
        );
    }
}
