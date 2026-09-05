//! Data-driven NDJSON ACP child used by launcher and scenario-harness tests.

use std::{
    env, fs,
    io::{self, BufRead as _, Write as _},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::Duration,
};

use serde::Deserialize;
use serde_json::{Map, Value, json};

const DEFAULT_SESSION_ID: &str = "11111111-2222-4333-8444-555555555555";

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScriptedLog {
    file: PathBuf,
    lines: Vec<Value>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum PromptBehavior {
    #[default]
    Respond,
    Error,
    HangUntilCancel,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
#[allow(clippy::struct_excessive_bools)] // Mirrors the source behavior.json wire exactly.
struct Behavior {
    fail_on_boot: bool,
    reject_new_session: bool,
    reject_extra_dirs: bool,
    prompt: PromptBehavior,
    persist_logs_on_cancel: bool,
    permission_probe: bool,
    echo_env: bool,
    #[serde(skip)]
    launcher_environment_probe: bool,
    echo_workspace: bool,
    stderr_note: Option<String>,
    late_inherited_output: bool,
    logs: Vec<ScriptedLog>,
    stray_root_file: bool,
    stray_bucket_file: bool,
    delete_sessions_root: bool,
}

struct FixtureAgent {
    behavior: Behavior,
    sessions_root: PathBuf,
    session_id: String,
    session_id_override: Option<String>,
    next_session_number: u64,
    session_cwd: PathBuf,
    parked_prompt: Option<Value>,
    parked_turn_log: Option<PathBuf>,
    pending_permission_id: Option<u64>,
    next_outbound_id: u64,
}

fn main() -> anyhow::Result<()> {
    if env::args().any(|argument| argument == "--late-child") {
        return late_child();
    }
    let fixture_file = env_path("SEEKDEEP_SNAPSHOT_FILE");
    let behavior = match fixture_file.as_deref() {
        Some(fixture_file) => {
            let behavior_file = fixture_file
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .join("behavior.json");
            serde_json::from_slice(&fs::read(behavior_file)?)?
        }
        None => Behavior::default(),
    };
    if let Some(note) = &behavior.stderr_note {
        eprintln!("{note}");
    }
    if env::var_os("SEEKDEEP_ACP_FIXTURE_STDERR").is_some() {
        eprintln!("{}", env::var("SEEKDEEP_ACP_FIXTURE_STDERR")?);
    }
    if behavior.fail_on_boot || env::var_os("SEEKDEEP_ACP_FIXTURE_FAIL_BOOT").is_some() {
        std::process::exit(7);
    }
    let behavior = merge_probe_environment(behavior);
    let mut agent = FixtureAgent {
        behavior,
        sessions_root: env_path("SEEKDEEP_SNAPSHOT_SESSIONS_ROOT").unwrap_or_default(),
        session_id: String::new(),
        session_id_override: env::var("SEEKDEEP_ACP_FIXTURE_SESSION_ID").ok(),
        next_session_number: 1,
        session_cwd: PathBuf::new(),
        parked_prompt: None,
        parked_turn_log: None,
        pending_permission_id: None,
        next_outbound_id: 1_000,
    };

    for line in io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        agent.handle_frame(&serde_json::from_str(&line)?)?;
    }
    agent.flush_logs_and_exit()
}

fn merge_probe_environment(mut behavior: Behavior) -> Behavior {
    behavior.permission_probe |= env::var_os("SEEKDEEP_ACP_FIXTURE_PERMISSION").is_some();
    behavior.launcher_environment_probe = env::var_os("SEEKDEEP_ACP_FIXTURE_ECHO_ENV").is_some();
    behavior.echo_env |= behavior.launcher_environment_probe;
    behavior.late_inherited_output |= env::var_os("SEEKDEEP_ACP_FIXTURE_LATE_OUTPUT").is_some();
    if env::var_os("SEEKDEEP_ACP_FIXTURE_HANG_PROMPT").is_some() {
        behavior.prompt = PromptBehavior::HangUntilCancel;
    }
    behavior
}

impl FixtureAgent {
    fn handle_frame(&mut self, frame: &Value) -> anyhow::Result<()> {
        let method = frame.get("method").and_then(Value::as_str);
        let id = frame.get("id").cloned();
        if method.is_none() && id.as_ref().and_then(Value::as_u64) == self.pending_permission_id {
            self.handle_permission_response(frame)?;
            return Ok(());
        }
        match method {
            Some("initialize") => respond(
                id,
                json!({"protocolVersion":1,"agentCapabilities":{"loadSession":false}}),
            ),
            Some("session/new") => self.handle_new_session(id, frame),
            Some("session/prompt") => self.handle_prompt(id),
            Some("session/cancel") => self.handle_cancel(),
            Some(method) if id.is_some() => {
                respond_error(id, &format!("unhandled method {method}"))
            }
            Some(_) | None => Ok(()),
        }
    }

    fn handle_new_session(&mut self, id: Option<Value>, frame: &Value) -> anyhow::Result<()> {
        let params = frame.get("params").and_then(Value::as_object);
        let extra = params
            .and_then(|params| params.get("additionalDirectories"))
            .and_then(Value::as_array);
        if self.behavior.reject_new_session
            || (self.behavior.reject_extra_dirs && extra.is_some_and(|extra| !extra.is_empty()))
        {
            return respond_error(id, "unsupported workspace scope");
        }
        if let Some(cwd) = params
            .and_then(|params| params.get("cwd"))
            .and_then(Value::as_str)
        {
            self.session_cwd = PathBuf::from(cwd);
        } else {
            self.session_cwd = env::current_dir()?;
        }
        self.session_id = self.session_id_override.take().unwrap_or_else(|| {
            let id = deterministic_session_id(self.next_session_number);
            self.next_session_number += 1;
            id
        });
        respond(id, json!({"sessionId":self.session_id}))
    }

    fn handle_prompt(&mut self, id: Option<Value>) -> anyhow::Result<()> {
        update(&self.session_id, "thinking about it")?;
        if self.behavior.echo_env {
            let mut values = Map::from_iter([
                ("mode".to_owned(), json!(env::var("SEEKDEEP_SNAPSHOT").ok())),
                (
                    "override".to_owned(),
                    json!(env::var("SEEKDEEP_SNAPSHOT_OVERRIDE").ok()),
                ),
                (
                    "childFiles".to_owned(),
                    json!(env::var("SEEKDEEP_SNAPSHOT_CHILD_FILES").ok()),
                ),
                (
                    "spillRoot".to_owned(),
                    json!(env::var("SEEKDEEP_SNAPSHOT_SPILL_ROOT").ok()),
                ),
                (
                    "permissionMode".to_owned(),
                    json!(env::var("SEEKDEEP_PERMISSION_MODE").ok()),
                ),
            ]);
            if self.behavior.launcher_environment_probe {
                values.insert("home".to_owned(), json!(env::var("SEEKDEEP_HOME").ok()));
                values.insert(
                    "agentsHome".to_owned(),
                    json!(env::var("SEEKDEEP_AGENTS_HOME").ok()),
                );
                values.insert(
                    "custom".to_owned(),
                    json!(env::var("SEEKDEEP_ACP_FIXTURE_CUSTOM").ok()),
                );
            }
            update(&self.session_id, &format!("env:{}", Value::Object(values)))?;
        }
        if self.behavior.echo_workspace {
            let mut entries = fs::read_dir(env::current_dir()?)?
                .filter_map(Result::ok)
                .filter_map(|entry| entry.file_name().into_string().ok())
                .collect::<Vec<_>>();
            entries.sort();
            update(
                &self.session_id,
                &format!("workspace:{}", entries.join(",")),
            )?;
        }
        if self.behavior.permission_probe {
            self.parked_prompt = id;
            let request_id = self.next_outbound_id;
            self.next_outbound_id += 1;
            self.pending_permission_id = Some(request_id);
            return send(json!({
                "id":request_id,
                "method":"session/request_permission",
                "params":{
                    "sessionId":self.session_id,
                    "toolCall":{"toolCallId":"call_fake_1"},
                    "options":[
                        {"optionId":"opt-allow","name":"Allow once","kind":"allow_once"},
                        {"optionId":"opt-reject","name":"Reject once","kind":"reject_once"}
                    ]
                }
            }));
        }
        self.settle_prompt(id)
    }

    fn handle_permission_response(&mut self, frame: &Value) -> anyhow::Result<()> {
        if self.pending_permission_id.take().is_none() {
            return Ok(());
        }
        let outcome = frame
            .pointer("/result/outcome")
            .cloned()
            .unwrap_or(Value::Null);
        update(&self.session_id, &format!("permission:{outcome}"))?;
        let prompt = self.parked_prompt.take();
        self.settle_prompt(prompt)
    }

    fn settle_prompt(&mut self, id: Option<Value>) -> anyhow::Result<()> {
        match self.behavior.prompt {
            PromptBehavior::Respond => respond(id, json!({"stopReason":"end_turn"})),
            PromptBehavior::Error => respond_error(id, "model exploded"),
            PromptBehavior::HangUntilCancel => {
                self.persist_parked_turn_start()?;
                self.parked_prompt = id;
                Ok(())
            }
        }
    }

    fn handle_cancel(&mut self) -> anyhow::Result<()> {
        let Some(prompt) = self.parked_prompt.take() else {
            return Ok(());
        };
        self.clear_parked_turn_start()?;
        if self.behavior.persist_logs_on_cancel {
            self.write_logs()?;
        }
        respond(Some(prompt), json!({"stopReason":"cancelled"}))
    }

    fn persist_parked_turn_start(&mut self) -> anyhow::Result<()> {
        let target = self
            .sessions_root
            .join("ready")
            .join(&self.session_id)
            .join("session.jsonl");
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(
            &target,
            format!(
                "{}\n{}\n",
                json!({
                    "type":"session","version":0,"id":self.session_id,
                    "createdAt":1,"cwd":self.session_cwd,"delegationDepth":0
                }),
                json!({"type":"turn/start","seq":0,"time":1,"data":{"turn":1}})
            ),
        )?;
        self.parked_turn_log = Some(target);
        Ok(())
    }

    fn clear_parked_turn_start(&mut self) -> anyhow::Result<()> {
        let Some(target) = self.parked_turn_log.take() else {
            return Ok(());
        };
        match fs::remove_file(target) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn write_logs(&self) -> anyhow::Result<()> {
        for log in &self.behavior.logs {
            let target = self.sessions_root.join(&log.file);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let content = log
                .lines
                .iter()
                .map(|line| instantiate(line.clone(), &self.session_cwd, &self.session_id))
                .map(|line| serde_json::to_string(&line))
                .collect::<Result<Vec<_>, _>>()?
                .join("\n");
            fs::write(target, format!("{content}\n"))?;
        }
        Ok(())
    }

    fn flush_logs_and_exit(mut self) -> anyhow::Result<()> {
        self.clear_parked_turn_start()?;
        self.write_logs()?;
        if self.behavior.stray_root_file {
            fs::create_dir_all(&self.sessions_root)?;
            fs::write(self.sessions_root.join("stray.txt"), "not a bucket\n")?;
        }
        if self.behavior.stray_bucket_file {
            let bucket = self.sessions_root.join("bucket-noise");
            fs::create_dir_all(&bucket)?;
            fs::write(bucket.join("notes.txt"), "not a session log\n")?;
        }
        if self.behavior.delete_sessions_root {
            match fs::remove_dir_all(&self.sessions_root) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        if self.behavior.late_inherited_output {
            Command::new(env::current_exe()?)
                .arg("--late-child")
                .env("SEEKDEEP_ACP_FIXTURE_SESSION_ID", &self.session_id)
                .stdin(Stdio::null())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()?;
        }
        Ok(())
    }
}

fn deterministic_session_id(number: u64) -> String {
    // The source fake uses random UUIDs. The Rust port preserves distinct,
    // UUID-shaped issued identities without crossing the no-ambient-randomness
    // boundary; the harness normalizes their volatile value either way.
    format!("11111111-2222-4333-8444-{number:012x}")
}

fn instantiate(value: Value, cwd: &Path, session_id: &str) -> Value {
    match value {
        Value::String(value) => Value::String(
            value
                .replace("{{CWD}}", &cwd.to_string_lossy())
                .replace("{{SID}}", session_id),
        ),
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|value| instantiate(value, cwd, session_id))
                .collect(),
        ),
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, instantiate(value, cwd, session_id)))
                .collect(),
        ),
        value @ (Value::Null | Value::Bool(_) | Value::Number(_)) => value,
    }
}

fn late_child() -> anyhow::Result<()> {
    let session_id = env::var("SEEKDEEP_ACP_FIXTURE_SESSION_ID")
        .unwrap_or_else(|_| DEFAULT_SESSION_ID.to_owned());
    thread::sleep(Duration::from_millis(50));
    update(&session_id, "late inherited stdout")?;
    thread::sleep(Duration::from_millis(25));
    eprintln!("late inherited stderr");
    Ok(())
}

fn env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn update(session_id: &str, text: &str) -> anyhow::Result<()> {
    send(json!({
        "method":"session/update",
        "params":{
            "sessionId":session_id,
            "update":{
                "sessionUpdate":"agent_message_chunk",
                "content":{"type":"text","text":text}
            }
        }
    }))
}

fn respond(id: Option<Value>, result: Value) -> anyhow::Result<()> {
    let mut frame = Map::new();
    frame.insert("id".to_owned(), id.unwrap_or(Value::Null));
    frame.insert("result".to_owned(), result);
    send(Value::Object(frame))
}

fn respond_error(id: Option<Value>, message: &str) -> anyhow::Result<()> {
    let mut frame = Map::new();
    frame.insert("id".to_owned(), id.unwrap_or(Value::Null));
    frame.insert("error".to_owned(), json!({"code":-32603,"message":message}));
    send(Value::Object(frame))
}

fn send(frame: Value) -> anyhow::Result<()> {
    let Value::Object(frame) = frame else {
        anyhow::bail!("fixture ACP frame must be an object");
    };
    let mut envelope = Map::new();
    envelope.insert("jsonrpc".to_owned(), Value::String("2.0".to_owned()));
    envelope.extend(frame);
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    serde_json::to_writer(&mut stdout, &Value::Object(envelope))?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}
