//! Deterministic subprocess-seam failure and spawn-contract parity.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use seekdeep_cordis::Context;
use seekdeep_llm::{AbortSignal, CallId};
use seekdeep_subprocess::{
    ProcessId, ProcessSignal, SubprocessCollectedOutputs, SubprocessHandle, SubprocessHandleRef,
    SubprocessInput, SubprocessLookupEnvironment, SubprocessOutcome, SubprocessOutput,
    SubprocessOutputRead, SubprocessOutputReader, SubprocessRuntime, SubprocessService,
    SubprocessSpawnSpec, SubprocessTerminalHandleRef, SubprocessTerminalSpawnSpec,
};
use seekdeep_tool_fs_search::{Config, SearchErrorCode, apply};
use seekdeep_tools::{
    ToolExecutionInput, ToolExecutionResult, ToolPresentationMode, ToolRuntime, ToolRuntimeConfig,
};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
enum Mode {
    Success {
        stdout: String,
        lossy: bool,
    },
    Exit {
        code: i32,
        stderr: String,
        lossy: bool,
    },
    Signal,
    NullExit,
    DoneFailure,
    MissingStreams,
}

#[derive(Debug)]
struct Reader(SubprocessOutputRead);
impl SubprocessOutputReader for Reader {
    fn read_from(&self, _from_byte: u64) -> SubprocessOutputRead {
        self.0.clone()
    }
}

#[derive(Debug)]
struct Handle {
    mode: Mode,
}

#[async_trait]
impl SubprocessHandle for Handle {
    fn pid(&self) -> ProcessId {
        ProcessId::new(1)
    }
    fn stdin(&self) -> Option<SubprocessInput> {
        None
    }
    fn stdout(&self) -> Option<SubprocessOutput> {
        None
    }
    fn stderr(&self) -> Option<SubprocessOutput> {
        None
    }
    fn collected(&self) -> SubprocessCollectedOutputs {
        if matches!(self.mode, Mode::MissingStreams) {
            return SubprocessCollectedOutputs::default();
        }
        let (stdout, stderr, stderr_lossy) = match &self.mode {
            Mode::Success { stdout, lossy } => (
                SubprocessOutputRead {
                    text: stdout.clone(),
                    lossy: *lossy,
                    ..Default::default()
                },
                SubprocessOutputRead::default(),
                false,
            ),
            Mode::Exit { stderr, lossy, .. } => (
                SubprocessOutputRead::default(),
                SubprocessOutputRead {
                    text: stderr.clone(),
                    lossy: *lossy,
                    ..Default::default()
                },
                *lossy,
            ),
            _ => (
                SubprocessOutputRead::default(),
                SubprocessOutputRead::default(),
                false,
            ),
        };
        let _ = stderr_lossy;
        SubprocessCollectedOutputs {
            stdout: Some(Arc::new(Reader(stdout))),
            stderr: Some(Arc::new(Reader(stderr))),
        }
    }
    async fn done(&self) -> anyhow::Result<SubprocessOutcome> {
        match &self.mode {
            Mode::Success { .. } | Mode::MissingStreams => Ok(SubprocessOutcome {
                exit_code: Some(0),
                signal: None,
            }),
            Mode::Exit { code, .. } => Ok(SubprocessOutcome {
                exit_code: Some(*code),
                signal: None,
            }),
            Mode::Signal => Ok(SubprocessOutcome {
                exit_code: None,
                signal: Some(ProcessSignal::new("SIGKILL")),
            }),
            Mode::NullExit => Ok(SubprocessOutcome {
                exit_code: None,
                signal: None,
            }),
            Mode::DoneFailure => anyhow::bail!("spawn failed"),
        }
    }
    fn terminate(&self) {}
    async fn wait_for_exit(&self, _signal: Option<AbortSignal>) -> bool {
        true
    }
}

#[derive(Debug)]
struct Runtime {
    mode: Mutex<Mode>,
    specs: Mutex<Vec<SubprocessSpawnSpec>>,
}

#[async_trait]
impl SubprocessRuntime for Runtime {
    async fn resolve_executable(
        &self,
        _command: &str,
        _env: Option<&SubprocessLookupEnvironment>,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<String> {
        Ok("/fake/rg".into())
    }
    fn spawn(&self, spec: SubprocessSpawnSpec) -> anyhow::Result<SubprocessHandleRef> {
        self.specs.lock().unwrap().push(spec);
        Ok(Arc::new(Handle {
            mode: self.mode.lock().unwrap().clone(),
        }))
    }
    async fn spawn_terminal(
        &self,
        _spec: SubprocessTerminalSpawnSpec,
    ) -> anyhow::Result<SubprocessTerminalHandleRef> {
        anyhow::bail!("not used")
    }
}

struct Harness {
    tools: Arc<ToolRuntime>,
    runtime: Arc<Runtime>,
}

fn harness(mode: Mode) -> Harness {
    let context = Context::new();
    let prompt = seekdeep_system_prompt::install(
        &context,
        seekdeep_system_prompt::SystemPromptConfig::default(),
    )
    .unwrap();
    let tools = seekdeep_tools::install(
        &context,
        &prompt,
        ToolRuntimeConfig {
            mode: ToolPresentationMode::Native,
            ..Default::default()
        },
    )
    .unwrap();
    let runtime = Arc::new(Runtime {
        mode: Mutex::new(mode),
        specs: Mutex::new(Vec::new()),
    });
    SubprocessService::new(runtime.clone())
        .provide(&context)
        .unwrap();
    apply(
        &context,
        &Config {
            sample_over_cap_glob_results: Some(false),
            raw_output_max_bytes: Some(8),
            stderr_max_bytes: Some(9),
            grace_ms: Some(10.0),
            ..Config::default()
        },
    )
    .unwrap();
    Harness { tools, runtime }
}

async fn call(harness: &Harness, name: &str, arguments: Value) -> ToolExecutionResult {
    harness
        .tools
        .execute(ToolExecutionInput::new(
            CallId::new("call"),
            name,
            arguments,
            AbortSignal::default(),
        ))
        .await
}

fn code(result: &ToolExecutionResult) -> Option<&str> {
    result
        .error()
        .and_then(|error| error.info.as_ref())
        .map(|info| info.code.as_str())
}

#[tokio::test]
async fn spawn_spec_and_missing_collectors_are_exact_and_fail_typed() {
    let harness = harness(Mode::MissingStreams);
    let result = call(&harness, "glob", json!({"pattern":"*.rs", "path":"-root"})).await;
    assert_eq!(code(&result), Some(SearchErrorCode::Failed.as_str()));
    let specs = harness.runtime.specs.lock().unwrap();
    assert_eq!(
        &specs[0].argv[..4],
        ["/fake/rg", "--no-config", "--files", "--glob=*.rs"]
    );
    assert_eq!(&specs[0].argv[specs[0].argv.len() - 2..], ["--", "-root"]);
    assert!((specs[0].grace_ms - 10.0).abs() < f64::EPSILON);
    assert!(specs[0].signal.is_some());
}

#[tokio::test]
async fn every_exit_and_transport_failure_maps_to_the_search_vocabulary() {
    let harness = harness(Mode::Exit {
        code: 2,
        stderr: "regex parse error".into(),
        lossy: false,
    });
    assert_eq!(
        code(&call(&harness, "grep", json!({"pattern":"["})).await),
        Some(SearchErrorCode::InvalidPattern.as_str())
    );
    *harness.runtime.mode.lock().unwrap() = Mode::Exit {
        code: 2,
        stderr: "permission denied".into(),
        lossy: true,
    };
    let failed = call(&harness, "glob", json!({"pattern":"*"})).await;
    assert_eq!(code(&failed), Some(SearchErrorCode::Failed.as_str()));
    assert!(failed.error().unwrap().message.contains("stderr truncated"));
    for mode in [Mode::Signal, Mode::NullExit, Mode::DoneFailure] {
        *harness.runtime.mode.lock().unwrap() = mode;
        assert_eq!(
            code(&call(&harness, "glob", json!({"pattern":"*"})).await),
            Some(SearchErrorCode::Failed.as_str())
        );
    }
}

#[tokio::test]
async fn lossy_and_over_cap_inline_stdout_fail_before_parsing() {
    for mode in [
        Mode::Success {
            stdout: "x".into(),
            lossy: true,
        },
        Mode::Success {
            stdout: "123456789".into(),
            lossy: false,
        },
    ] {
        let harness = harness(mode);
        assert_eq!(
            code(&call(&harness, "glob", json!({"pattern":"*"})).await),
            Some(SearchErrorCode::RawOutputOverflow.as_str())
        );
    }
}
