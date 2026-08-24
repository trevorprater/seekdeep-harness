//! Execute command hooks through the shell executor and decode their outcome.

use std::{collections::BTreeMap, path::PathBuf};

use seekdeep_llm::AbortSignal;
use seekdeep_shell::{ShellExecRequest, ShellExecutor, ShellRunResult};
use serde_json::Value;

use crate::codec::parse_hook_output;
use crate::types::{CommandHook, HookOutput};

/// The reference default per-hook timeout, in milliseconds (10 minutes).
pub const DEFAULT_HOOK_TIMEOUT_MS: u64 = 600_000;

/// Everything a single hook invocation needs beyond its command line.
#[derive(Clone, Debug)]
pub struct RunHookOptions {
    /// The JSON payload object written to the hook's stdin.
    pub payload: Value,
    /// Extra env vars for the hook process.
    pub env: Option<BTreeMap<String, String>>,
    /// Working directory for the hook.
    pub cwd: Option<String>,
    /// Explicit owning-operation signal; firing it cancels the hook run.
    pub signal: AbortSignal,
    /// Whether to append a trailing newline to the stdin payload.
    pub trailing_newline: bool,
    /// Timeout applied when the hook's config sets none of its own.
    pub default_timeout_ms: f64,
    /// The event this hook fires for, guarding mismatched hookSpecificOutput blocks.
    pub expected_event_name: Option<String>,
}

/// The decoded outcome plus the wall-clock duration of the run.
#[derive(Clone, Debug)]
pub struct RunHookResult {
    /// The decoded hook outcome.
    pub output: HookOutput,
    /// Wall-clock duration of the run, in milliseconds.
    pub duration_ms: i64,
}

/// Runs a hook with serialized stdin and decodes its outcome.
///
/// Infrastructure rejection becomes an outcome with no exit code, so this
/// function never throws or crashes the calling turn.
pub async fn run_hook(
    bash: &dyn ShellExecutor,
    hook: &CommandHook,
    options: &RunHookOptions,
    mut now: impl FnMut() -> i64,
) -> RunHookResult {
    let started = now();
    let timeout_ms = hook
        .timeout_sec
        .map_or(options.default_timeout_ms, |sec| sec * 1000.0);
    let stdin = options.payload.to_string() + if options.trailing_newline { "\n" } else { "" };

    let mut request = ShellExecRequest::new(hook.command.clone());
    request.timeout_ms = Some(timeout_ms);
    request.stdin = Some(stdin);
    request.signal = Some(options.signal.clone());
    if let Some(cwd) = &options.cwd {
        request.workdir = Some(PathBuf::from(cwd));
    }
    if let Some(env) = &options.env {
        request.env = Some(env.clone());
    }

    let result: anyhow::Result<ShellRunResult> = match bash.resolve(request) {
        Ok(spec) => bash.run(spec).await,
        Err(error) => Err(error),
    };
    let duration_ms = now() - started;
    match result {
        Ok(result) => RunHookResult {
            output: parse_hook_output(
                result.exit_code,
                &result.stdout.text,
                &result.stderr.text,
                options.expected_event_name.as_deref(),
            ),
            duration_ms,
        },
        Err(error) => RunHookResult {
            output: parse_hook_output(None, "", &error.to_string(), None),
            duration_ms,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use parking_lot::Mutex;
    use seekdeep_shell::{CollectedOutput, ProcessSignal, ShellExecSpec, ShellProcessHandle};
    use serde_json::json;

    use super::*;

    #[derive(Debug)]
    enum Behavior {
        Ok(ShellRunResult),
        Err(String),
    }

    #[derive(Debug)]
    struct RecordingBash {
        specs: Arc<Mutex<Vec<ShellExecSpec>>>,
        behavior: Behavior,
    }

    impl RecordingBash {
        fn ok(result: ShellRunResult) -> Self {
            Self {
                specs: Arc::new(Mutex::new(Vec::new())),
                behavior: Behavior::Ok(result),
            }
        }

        fn err(message: impl Into<String>) -> Self {
            Self {
                specs: Arc::new(Mutex::new(Vec::new())),
                behavior: Behavior::Err(message.into()),
            }
        }

        fn specs(&self) -> Vec<ShellExecSpec> {
            self.specs.lock().clone()
        }
    }

    #[async_trait::async_trait]
    impl ShellExecutor for RecordingBash {
        fn resolve(&self, request: ShellExecRequest) -> anyhow::Result<ShellExecSpec> {
            Ok(ShellExecSpec {
                command: request.command,
                workdir: request.workdir.unwrap_or_else(|| PathBuf::from("/stub")),
                timeout_ms: request.timeout_ms.unwrap_or(0.0),
                stdout_max_bytes: request.stdout_max_bytes.unwrap_or(64_000.0),
                signal: request.signal,
                stdin: request.stdin,
                env: request.env,
                seekdeep_env: request.seekdeep_env,
                sandbox_policy: request.sandbox_policy,
            })
        }

        async fn run(&self, spec: ShellExecSpec) -> anyhow::Result<ShellRunResult> {
            self.specs.lock().push(spec.clone());
            match &self.behavior {
                Behavior::Ok(result) => Ok(result.clone()),
                Behavior::Err(message) => Err(anyhow::anyhow!(message.clone())),
            }
        }

        fn start(&self, _spec: ShellExecSpec) -> anyhow::Result<ShellProcessHandle> {
            Err(anyhow::anyhow!(
                "start is not exercised by hook-protocol runner tests"
            ))
        }
    }

    fn collected(text: &str) -> CollectedOutput {
        CollectedOutput {
            text: text.to_owned(),
            truncated: false,
            spill_path: None,
        }
    }

    fn default_result() -> ShellRunResult {
        ShellRunResult {
            exit_code: Some(0),
            signal: None,
            timed_out: false,
            aborted: false,
            timeout_ms: 1000.0,
            stdout: collected(""),
            stderr: collected(""),
            sandbox: None,
        }
    }

    fn hook(command: &str) -> CommandHook {
        CommandHook {
            command: command.to_owned(),
            timeout_sec: None,
        }
    }

    fn options() -> RunHookOptions {
        RunHookOptions {
            payload: Value::Null,
            env: None,
            cwd: None,
            signal: AbortSignal::default(),
            trailing_newline: true,
            default_timeout_ms: 60_000.0,
            expected_event_name: None,
        }
    }

    fn clock() -> impl FnMut() -> i64 {
        let mut t = 0_i64;
        move || {
            t += 5;
            t
        }
    }

    #[tokio::test]
    async fn serializes_payload_to_stdin_with_trailing_newline() {
        let bash = RecordingBash::ok(default_result());
        let mut options = options();
        options.payload = json!({"hook_event_name": "PreToolUse", "tool_name": "Bash"});
        run_hook(&bash, &hook("my-hook.sh"), &options, clock()).await;
        let specs = bash.specs();
        assert_eq!(
            specs[0].stdin.as_deref(),
            Some("{\"hook_event_name\":\"PreToolUse\",\"tool_name\":\"Bash\"}\n")
        );
        assert_eq!(specs[0].command, "my-hook.sh");
    }

    #[tokio::test]
    async fn omits_trailing_newline_when_requested() {
        let bash = RecordingBash::ok(default_result());
        let mut options = options();
        options.payload = json!({"a": 1});
        options.trailing_newline = false;
        run_hook(&bash, &hook("h"), &options, clock()).await;
        assert_eq!(bash.specs()[0].stdin.as_deref(), Some("{\"a\":1}"));
    }

    #[tokio::test]
    async fn threads_env_and_cwd_into_the_request() {
        let bash = RecordingBash::ok(default_result());
        let mut options = options();
        options.env = Some(BTreeMap::from([(
            "CLAUDE_PROJECT_DIR".to_owned(),
            "/proj".to_owned(),
        )]));
        options.cwd = Some("/work".to_owned());
        run_hook(&bash, &hook("h"), &options, clock()).await;
        let specs = bash.specs();
        assert_eq!(
            specs[0]
                .env
                .as_ref()
                .and_then(|env| env.get("CLAUDE_PROJECT_DIR")),
            Some(&"/proj".to_owned())
        );
        assert_eq!(specs[0].workdir, PathBuf::from("/work"));
    }

    #[tokio::test]
    async fn per_hook_timeout_sec_overrides_default() {
        let bash = RecordingBash::ok(default_result());
        let mut hook = hook("h");
        hook.timeout_sec = Some(3.0);
        run_hook(&bash, &hook, &options(), clock()).await;
        assert!((bash.specs()[0].timeout_ms - 3000.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn falls_back_to_default_timeout_and_constant_matches_source() {
        let bash = RecordingBash::ok(default_result());
        run_hook(&bash, &hook("h"), &options(), clock()).await;
        assert!((bash.specs()[0].timeout_ms - 60_000.0).abs() < f64::EPSILON);
        assert_eq!(DEFAULT_HOOK_TIMEOUT_MS, 600_000);
    }

    #[tokio::test]
    async fn passes_the_abort_signal_through() {
        let bash = RecordingBash::ok(default_result());
        let signal = AbortSignal::default();
        let mut options = options();
        options.signal = signal.clone();
        run_hook(&bash, &hook("h"), &options, clock()).await;
        assert_eq!(bash.specs()[0].signal.as_ref(), Some(&signal));
    }

    #[tokio::test]
    async fn decodes_clean_exit_with_structured_stdout_and_duration() {
        let mut result = default_result();
        result.stdout = collected("{\"decision\":\"block\",\"reason\":\"no\"}");
        let bash = RecordingBash::ok(result);
        let outcome = run_hook(&bash, &hook("h"), &options(), clock()).await;
        assert_eq!(
            outcome.output.decision,
            Some(crate::types::HookDecision::Block)
        );
        assert_eq!(outcome.output.reason.as_deref(), Some("no"));
        assert_eq!(outcome.duration_ms, 5);
    }

    #[tokio::test]
    async fn signal_death_decodes_as_undefined_exit() {
        let mut result = default_result();
        result.exit_code = None;
        result.signal = Some(ProcessSignal::new("SIGKILL"));
        result.stderr = collected("killed");
        let bash = RecordingBash::ok(result);
        let outcome = run_hook(&bash, &hook("h"), &options(), clock()).await;
        assert_eq!(outcome.output.exit_code, None);
        assert_eq!(outcome.output.decision, None);
        assert_eq!(outcome.output.stderr, "killed");
    }

    #[tokio::test]
    async fn executor_rejection_becomes_a_non_blocking_error() {
        let bash = RecordingBash::err("bad workdir: ENOENT");
        let outcome = run_hook(&bash, &hook("h"), &options(), clock()).await;
        assert_eq!(outcome.output.exit_code, None);
        assert_eq!(outcome.output.stderr, "bad workdir: ENOENT");
        assert_eq!(outcome.output.decision, None);
    }

    #[tokio::test]
    async fn a_non_error_rejection_is_stringified_onto_stderr() {
        let bash = RecordingBash::err("plain string fault");
        let outcome = run_hook(&bash, &hook("h"), &options(), clock()).await;
        assert_eq!(outcome.output.stderr, "plain string fault");
    }

    #[tokio::test]
    async fn threads_expected_event_name_to_discard_mismatched_blocks() {
        let mut result = default_result();
        result.stdout = collected(
            "{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"deny\"}}",
        );
        let bash = RecordingBash::ok(result);
        let mut options = options();
        options.expected_event_name = Some("Stop".to_owned());
        let outcome = run_hook(&bash, &hook("h"), &options, clock()).await;
        assert_eq!(
            outcome.output.hook_event_name.as_deref(),
            Some("PreToolUse")
        );
        assert_eq!(outcome.output.decision, None);
    }
}
