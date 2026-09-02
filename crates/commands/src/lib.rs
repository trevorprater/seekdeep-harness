//! Scoped human-command registry and direct, durably paired execution.

/// Package-owned command lifecycle invariants.
pub mod invariant;

use std::{
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use futures::{FutureExt, future::Either};
use regex::Regex;
use seekdeep_agent::Agent;
use seekdeep_api_gateway::register_invocable_service_if_available;
pub use seekdeep_commands_contract::{CommandDescriptor, CommandInputDescriptor};
use seekdeep_cordis::{Context, EventArgs, Plugin, ServiceKey, fiber::EffectHandle};
use seekdeep_core::session::{AppendOptions, Session};
use seekdeep_llm::AbortSignal;
use seekdeep_scope::{
    ScopeKey,
    store::{LayerEffectOptions, NamedEntries, ScopeLayer, ScopedLayers},
};
use seekdeep_typert_protocol::{
    InvocationDescriptor, InvocationParameterDescriptor, InvocationParameterSource,
    InvocationReceiver, InvocationScope, InvocationSourceLocation, RemoteMethodMarker,
    TypertBoundaryValue, TypertCodec, TypertHostArgument, TypertInvocableService,
    TypertInvocationFuture, TypertRemoteContribution, TypertRemoteService, TypertSchema,
    typert_remote_method,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use uuid::Uuid;

/// Typed Cordis slot corresponding to `ctx.commands`.
pub const COMMANDS: ServiceKey<CommandRuntime> = ServiceKey::new("commands");
/// Loader plugin identity.
pub const PLUGIN_NAME: &str = "commands";
/// Command registry has no service prerequisites.
pub const PLUGIN_INJECT: &[&str] = &[];

/// Maximum JavaScript safe integer accepted for a domain-event reference.
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Branded command lifecycle pairing identity.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CommandId(String);

impl CommandId {
    /// Brands one string without validation.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Exact wire string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CommandId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Merge-extensible producer record for one human-issued command.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CommandSource {
    /// Source tag, currently `user`.
    pub kind: String,
    /// Future source fields retained losslessly.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl CommandSource {
    /// Current human-facing UI source.
    #[must_use]
    pub fn user() -> Self {
        Self {
            kind: "user".to_owned(),
            extra: Map::new(),
        }
    }
}

/// Expected command outcome rendered directly by the dispatching UI.
///
/// JavaScript's malformed-handler cases are excluded at this Rust boundary:
///
/// ```compile_fail
/// use seekdeep_commands::CommandResult;
/// let result: CommandResult = serde_json::json!({"kind": "future"});
/// # let _ = result;
/// ```
///
/// ```compile_fail
/// use seekdeep_commands::CommandResult;
/// let result = CommandResult::Error { text: 1 };
/// # let _ = result;
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum CommandResult {
    /// Successful direct operation.
    Success {
        /// Optional UI text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        /// Earlier authoritative domain-event sequence.
        #[serde(
            rename = "sourceEventSeq",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        source_event_seq: Option<u64>,
    },
    /// Expected, directly rendered rejection.
    Error {
        /// Non-empty UI text.
        text: String,
    },
}

impl CommandResult {
    /// Builds a successful outcome.
    #[must_use]
    pub fn success(text: Option<impl Into<String>>) -> Self {
        Self::Success {
            text: text.map(Into::into),
            source_event_seq: None,
        }
    }

    /// Builds success linked to an earlier authoritative event.
    #[must_use]
    pub fn success_linked(text: Option<impl Into<String>>, source_event_seq: u64) -> Self {
        Self::Success {
            text: text.map(Into::into),
            source_event_seq: Some(source_event_seq),
        }
    }

    /// Builds an expected error outcome.
    #[must_use]
    pub fn error(text: impl Into<String>) -> Self {
        Self::Error { text: text.into() }
    }

    /// Closed kind spelling.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Success { .. } => "success",
            Self::Error { .. } => "error",
        }
    }

    /// Optional text for the UI and durable settlement.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Success { text, .. } => text.as_deref(),
            Self::Error { text } => Some(text),
        }
    }

    /// Earlier authoritative domain event, on success only.
    #[must_use]
    pub const fn source_event_seq(&self) -> Option<u64> {
        match self {
            Self::Success {
                source_event_seq, ..
            } => *source_event_seq,
            Self::Error { .. } => None,
        }
    }
}

/// One settled execution and its durable pairing ID.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandExecution {
    /// Pairing identity carried by run and done records.
    pub command_id: CommandId,
    /// Normalized outcome.
    pub result: CommandResult,
}

/// Parsed syntactically valid slash command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedCommand {
    /// Lowercase name without slash.
    pub name: String,
    /// Exact text following the name, separator included.
    pub raw_input: String,
}

/// Exact handler invocation context.
#[derive(Clone, Debug)]
pub struct CommandInvocation {
    /// Already-written lifecycle pairing ID.
    pub command_id: CommandId,
    /// Exact receiving agent.
    pub agent: Arc<Agent>,
    /// Verbatim suffix including separator whitespace.
    pub raw_input: String,
    /// Dispatching UI cancellation signal.
    pub signal: AbortSignal,
}

/// Boxed direct command-handler future.
pub type CommandFuture =
    Pin<Box<dyn Future<Output = anyhow::Result<CommandResult>> + Send + 'static>>;
/// Direct command handler.
pub type CommandHandler = Arc<dyn Fn(CommandInvocation) -> CommandFuture + Send + Sync + 'static>;

/// Plugin-owned command registration.
#[derive(Clone)]
pub struct CommandDefinition {
    /// Lowercase command name without slash.
    pub name: String,
    /// Human-readable discovery summary.
    pub description: String,
    /// Optional free-form input hint.
    pub input: Option<CommandInputDescriptor>,
    /// Whether `command/run` records raw input. Defaults to true.
    pub record_input: Option<bool>,
    /// Direct execution body.
    pub handler: CommandHandler,
}

impl std::fmt::Debug for CommandDefinition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CommandDefinition")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("input", &self.input)
            .field("record_input", &self.record_input)
            .finish_non_exhaustive()
    }
}

impl CommandDefinition {
    /// Builds mandatory metadata and handler.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        handler: CommandHandler,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input: None,
            record_input: None,
            handler,
        }
    }

    /// Adds a discovery input hint.
    #[must_use]
    pub fn with_input(mut self, hint: impl Into<String>) -> Self {
        self.input = Some(CommandInputDescriptor { hint: hint.into() });
        self
    }

    /// Selects durable raw-input recording.
    #[must_use]
    pub fn record_input(mut self, record: bool) -> Self {
        self.record_input = Some(record);
        self
    }
}

#[derive(Clone)]
struct RegisteredCommand {
    definition: CommandDefinition,
    descriptor: CommandDescriptor,
}

struct CommandLayer {
    commands: NamedEntries<RegisteredCommand>,
}

impl CommandLayer {
    fn new(scope: Option<ScopeKey>) -> Self {
        Self {
            commands: NamedEntries::new(move |name| {
                if scope.is_some() {
                    anyhow::anyhow!("command {name:?} is already registered in this scope")
                } else {
                    anyhow::anyhow!(
                        "command {name:?} is already registered (for a per-agent variant, mount a command-injected plugin under that agent's `agent.ctx`)"
                    )
                }
            }),
        }
    }
}

impl ScopeLayer for CommandLayer {
    fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

/// Human-command registry with global and agent-scoped shadows.
pub struct CommandRuntime {
    layers: ScopedLayers<CommandLayer>,
    command_seq: AtomicU64,
    instance_token: String,
}

impl std::fmt::Debug for CommandRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CommandRuntime")
            .field("instance_token", &self.instance_token)
            .field("command_seq", &self.command_seq.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl CommandRuntime {
    /// Constructs an unprovided runtime.
    #[must_use]
    pub fn new(context: &Context) -> Arc<Self> {
        let notify_context = context.clone();
        Arc::new(Self {
            layers: ScopedLayers::new(CommandLayer::new, move || {
                notify_change(&notify_context);
            }),
            command_seq: AtomicU64::new(0),
            instance_token: Uuid::new_v4().simple().to_string()[..8].to_owned(),
        })
    }

    /// Publishes this exact runtime on `ctx.commands`.
    ///
    /// # Errors
    ///
    /// Returns ordinary duplicate-service or inactive-owner failures.
    pub fn provide(
        self: &Arc<Self>,
        context: &Context,
    ) -> Result<EffectHandle, seekdeep_cordis::CordisError> {
        context.provide(COMMANDS, self.clone())
    }

    /// Registers a global or exact-scope command definition.
    ///
    /// # Errors
    ///
    /// Rejects invalid metadata, duplicates, or inactive ownership.
    pub fn register(
        &self,
        context: &Context,
        definition: CommandDefinition,
    ) -> anyhow::Result<EffectHandle> {
        let registered = normalize_definition(definition)?;
        let name = registered.definition.name.clone();
        self.layers.effect(
            context,
            move |layer| layer.commands.insert(name, registered),
            LayerEffectOptions::new("commands.register()"),
        )
    }

    /// Lists name-sorted effective immutable descriptors.
    #[must_use]
    pub fn list(&self, agent: &Agent) -> Vec<CommandDescriptor> {
        let mut descriptors = self
            .view(agent)
            .into_values()
            .map(|entry| entry.descriptor)
            .collect::<Vec<_>>();
        descriptors.sort_by(|left, right| left.name.cmp(&right.name));
        descriptors
    }

    /// Resolves one scoped shadow or global definition.
    #[must_use]
    pub fn find(&self, agent: &Agent, name: &str) -> Option<CommandDefinition> {
        self.view(agent)
            .get(name)
            .map(|entry| entry.definition.clone())
    }

    /// Parses and executes a known command without model dispatch.
    ///
    /// # Errors
    ///
    /// Returns cancellation, lifecycle append, handler, or result-validation
    /// failures. Syntax and resolution misses return `Ok(None)` without logs.
    pub async fn execute(
        &self,
        agent: Arc<Agent>,
        line: &str,
        signal: AbortSignal,
    ) -> anyhow::Result<Option<CommandExecution>> {
        let Some(parsed) = parse_command(line) else {
            return Ok(None);
        };
        let Some(command) = self.view(&agent).get(&parsed.name).cloned() else {
            return Ok(None);
        };
        if signal.is_aborted() {
            return Err(abort_error(&signal));
        }
        let command_id = self.mint_command_id();
        append_run(
            agent.session(),
            &command_id,
            &parsed,
            command.definition.record_input != Some(false),
        )?;
        let invocation = CommandInvocation {
            command_id: command_id.clone(),
            agent: agent.clone(),
            raw_input: parsed.raw_input,
            signal: signal.clone(),
        };
        let outcome = self
            .run_handler(&command.definition, invocation, &signal)
            .await;
        let result = match outcome {
            Ok(result) => normalize_result(&parsed.name, result),
            Err(error) => Err(error),
        };
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                if let Err(append_error) = append_done_error(agent.session(), &command_id, &error) {
                    tracing::warn!(command = %parsed.name, %append_error, "command/done append failed");
                }
                return Err(error);
            }
        };
        append_done(agent.session(), &command_id, &result)?;
        Ok(Some(CommandExecution { command_id, result }))
    }

    async fn run_handler(
        &self,
        definition: &CommandDefinition,
        invocation: CommandInvocation,
        signal: &AbortSignal,
    ) -> anyhow::Result<CommandResult> {
        let future = catch_unwind(AssertUnwindSafe(|| (definition.handler)(invocation)))
            .map_err(|panic| anyhow::anyhow!(panic_message(&panic)))?;
        let answer: Pin<Box<dyn Future<Output = anyhow::Result<CommandResult>> + Send + 'static>> =
            Box::pin(AssertUnwindSafe(future).catch_unwind().map(|result| {
                result
                    .map_err(|panic| anyhow::anyhow!(panic_message(&panic)))
                    .and_then(std::convert::identity)
            }));
        if signal.is_aborted() {
            detach_handler(answer);
            return Err(abort_error(signal));
        }
        let cancelled = signal.cancelled();
        match futures::future::select(answer, cancelled).await {
            Either::Left((result, _)) => result,
            Either::Right(((), late)) => {
                detach_handler(late);
                Err(abort_error(signal))
            }
        }
    }

    fn view(&self, agent: &Agent) -> indexmap::IndexMap<String, RegisteredCommand> {
        self.layers
            .merge(Some(agent.scope_key()), |layer| &layer.commands)
    }

    fn mint_command_id(&self) -> CommandId {
        let sequence = self.command_seq.fetch_add(1, Ordering::AcqRel) + 1;
        CommandId::new(format!("cmd-{}-{sequence}", self.instance_token))
    }
}

impl TypertRemoteService for CommandRuntime {
    fn typert_service_key(&self) -> &'static str {
        "commands"
    }

    fn remote_methods(&self) -> Vec<RemoteMethodMarker> {
        vec![
            typert_remote_method!(CommandRuntime, list),
            typert_remote_method!(CommandRuntime, execute),
        ]
    }
}

impl TypertInvocableService for CommandRuntime {
    fn service_key(&self) -> &'static str {
        "commands"
    }

    fn namespace(&self) -> &'static str {
        "commands"
    }

    fn remote_methods(&self) -> Vec<RemoteMethodMarker> {
        <Self as TypertRemoteService>::remote_methods(self)
    }

    fn parameter_names(&self, implementation: &str) -> Option<Vec<String>> {
        match implementation {
            "list" => Some(vec!["agent".to_owned()]),
            "execute" => Some(vec![
                "agent".to_owned(),
                "line".to_owned(),
                "signal".to_owned(),
            ]),
            _ => None,
        }
    }

    fn has_method(&self, implementation: &str) -> bool {
        matches!(implementation, "list" | "execute")
    }

    fn invoke(
        self: Arc<Self>,
        implementation: &str,
        arguments: Vec<TypertHostArgument>,
    ) -> TypertInvocationFuture {
        let implementation = implementation.to_owned();
        Box::pin(async move {
            match implementation.as_str() {
                "list" => {
                    anyhow::ensure!(arguments.len() == 1, "commands/list expects one argument");
                    let agent = agent_argument(&arguments[0])?;
                    Ok(TypertBoundaryValue::Json(serde_json::to_value(
                        self.list(&agent),
                    )?))
                }
                "execute" => {
                    anyhow::ensure!(
                        arguments.len() == 3,
                        "commands/execute expects three arguments"
                    );
                    let agent = agent_argument(&arguments[0])?;
                    let line = string_argument(&arguments[1])?;
                    let signal = signal_argument(&arguments[2])?;
                    self.execute(agent, &line, signal).await?.map_or_else(
                        || Ok(TypertBoundaryValue::Undefined),
                        |execution| Ok(TypertBoundaryValue::Json(serde_json::to_value(execution)?)),
                    )
                }
                _ => anyhow::bail!("commands has no callable method {implementation:?}"),
            }
        })
    }
}

fn agent_argument(argument: &TypertHostArgument) -> anyhow::Result<Arc<Agent>> {
    let TypertHostArgument::Lookup(agent) = argument else {
        anyhow::bail!("commands expected an Agent lookup argument");
    };
    agent
        .clone()
        .downcast::<Agent>()
        .map_err(|_| anyhow::anyhow!("commands lookup argument is not an Agent"))
}

fn string_argument(argument: &TypertHostArgument) -> anyhow::Result<String> {
    let TypertHostArgument::Boundary(TypertBoundaryValue::Json(Value::String(value))) = argument
    else {
        anyhow::bail!("commands expected a string boundary argument");
    };
    Ok(value.clone())
}

fn signal_argument(argument: &TypertHostArgument) -> anyhow::Result<AbortSignal> {
    let TypertHostArgument::Signal(signal) = argument else {
        anyhow::bail!("commands expected an AbortSignal argument");
    };
    Ok(signal.clone())
}

/// Generated strict Client/Host Remote descriptors for the Commands package.
#[must_use]
pub fn typert_remote_contribution() -> TypertRemoteContribution {
    TypertRemoteContribution {
        package: "@deepseek-ai/seekdeep-commands".to_owned(),
        descriptors: vec![execute_descriptor(), list_descriptor()],
    }
}

fn execute_descriptor() -> InvocationDescriptor {
    InvocationDescriptor {
        id: "@deepseek-ai/seekdeep-commands#commands/execute".to_owned(),
        service: "commands".to_owned(),
        namespace: "commands".to_owned(),
        method: "execute".to_owned(),
        implementation: None,
        invocation: InvocationReceiver::Direct,
        scope: Some(InvocationScope {
            context: "agent".to_owned(),
            wire: "agentId".to_owned(),
        }),
        parameters: vec![
            InvocationParameterDescriptor {
                name: "agent".to_owned(),
                wire: "agentId".to_owned(),
                source: InvocationParameterSource::Lookup,
                lookup: Some("agent".to_owned()),
                codec: strict(
                    "@deepseek-ai/seekdeep-session/types#SessionId",
                    Arc::new(StringBoundarySchema),
                ),
                accepts_undefined: None,
            },
            InvocationParameterDescriptor {
                name: "line".to_owned(),
                wire: "line".to_owned(),
                source: InvocationParameterSource::Json,
                lookup: None,
                codec: strict(
                    "@deepseek-ai/seekdeep-commands#commands/execute:line",
                    Arc::new(StringBoundarySchema),
                ),
                accepts_undefined: None,
            },
        ],
        cancellation: true,
        result: strict(
            "@deepseek-ai/seekdeep-commands#commands/execute:result",
            Arc::new(CommandExecutionBoundarySchema),
        ),
        source_location: Some(InvocationSourceLocation {
            file: "packages/interaction/commands/src/index.ts".to_owned(),
            line: 297,
            column: 9,
        }),
    }
}

fn list_descriptor() -> InvocationDescriptor {
    InvocationDescriptor {
        id: "@deepseek-ai/seekdeep-commands#commands/list".to_owned(),
        service: "commands".to_owned(),
        namespace: "commands".to_owned(),
        method: "list".to_owned(),
        implementation: None,
        invocation: InvocationReceiver::Direct,
        scope: Some(InvocationScope {
            context: "agent".to_owned(),
            wire: "agentId".to_owned(),
        }),
        parameters: vec![InvocationParameterDescriptor {
            name: "agent".to_owned(),
            wire: "agentId".to_owned(),
            source: InvocationParameterSource::Lookup,
            lookup: Some("agent".to_owned()),
            codec: strict(
                "@deepseek-ai/seekdeep-session/types#SessionId",
                Arc::new(StringBoundarySchema),
            ),
            accepts_undefined: None,
        }],
        cancellation: false,
        result: strict(
            "@deepseek-ai/seekdeep-commands#commands/list:result",
            Arc::new(CommandListBoundarySchema),
        ),
        source_location: Some(InvocationSourceLocation {
            file: "packages/interaction/commands/src/index.ts".to_owned(),
            line: 260,
            column: 3,
        }),
    }
}

fn strict(type_symbol: &str, schema: Arc<dyn TypertSchema>) -> TypertCodec {
    TypertCodec::Strict {
        type_symbol: type_symbol.to_owned(),
        schema,
    }
}

#[derive(Debug)]
struct StringBoundarySchema;

impl TypertSchema for StringBoundarySchema {
    fn parse(&self, value: TypertBoundaryValue) -> anyhow::Result<TypertBoundaryValue> {
        anyhow::ensure!(
            value.as_json().is_some_and(Value::is_string),
            "expected string"
        );
        Ok(value)
    }

    fn to_json_schema(&self) -> anyhow::Result<Value> {
        Ok(json!({"type": "string"}))
    }
}

#[derive(Debug)]
struct CommandExecutionBoundarySchema;

impl TypertSchema for CommandExecutionBoundarySchema {
    fn parse(&self, value: TypertBoundaryValue) -> anyhow::Result<TypertBoundaryValue> {
        let TypertBoundaryValue::Json(value) = value else {
            return Ok(TypertBoundaryValue::Undefined);
        };
        let execution = serde_json::from_value::<CommandExecution>(value)?;
        Ok(TypertBoundaryValue::Json(serde_json::to_value(execution)?))
    }

    fn to_json_schema(&self) -> anyhow::Result<Value> {
        Ok(json!({"oneOf": [
            {"type": "null", "description": "carrier omission represents undefined"},
            {"type": "object"}
        ]}))
    }
}

#[derive(Debug)]
struct CommandListBoundarySchema;

impl TypertSchema for CommandListBoundarySchema {
    fn parse(&self, value: TypertBoundaryValue) -> anyhow::Result<TypertBoundaryValue> {
        let TypertBoundaryValue::Json(value) = value else {
            anyhow::bail!("expected command descriptor array");
        };
        let descriptors = serde_json::from_value::<Vec<CommandDescriptor>>(value)?;
        Ok(TypertBoundaryValue::Json(serde_json::to_value(
            descriptors,
        )?))
    }

    fn to_json_schema(&self) -> anyhow::Result<Value> {
        Ok(json!({"type": "array", "items": {"type": "object"}}))
    }
}

/// Parses one exact slash command without normalizing its trailing input.
#[must_use]
pub fn parse_command(line: &str) -> Option<ParsedCommand> {
    let rest = line.strip_prefix('/')?;
    let mut bytes = rest.bytes();
    let first = bytes.next()?;
    if !first.is_ascii_lowercase() {
        return None;
    }
    let mut name_len = 1;
    for byte in bytes {
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-') {
            name_len += 1;
        } else {
            break;
        }
    }
    let suffix = &rest[name_len..];
    if !suffix.is_empty() && !matches!(suffix.as_bytes()[0], b'\t' | b'\n' | b'\r' | b' ') {
        return None;
    }
    let name = rest[..name_len].to_owned();
    let name_end = 1 + name.len();
    Some(ParsedCommand {
        name,
        raw_input: line[name_end..].to_owned(),
    })
}

fn normalize_definition(definition: CommandDefinition) -> anyhow::Result<RegisteredCommand> {
    static NAME: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let pattern = NAME
        .get_or_init(|| Regex::new(r"^[a-z][a-z0-9_-]*$").expect("constant command-name regex"));
    anyhow::ensure!(
        pattern.is_match(&definition.name),
        "command name {:?} must match /^[a-z][a-z0-9_-]*$/u",
        definition.name
    );
    anyhow::ensure!(
        !definition.description.trim().is_empty(),
        "command {:?} description must not be empty",
        definition.name
    );
    if let Some(input) = &definition.input {
        anyhow::ensure!(
            !input.hint.trim().is_empty(),
            "command {:?} input hint must not be empty",
            definition.name
        );
    }
    let descriptor = CommandDescriptor {
        name: definition.name.clone(),
        description: definition.description.clone(),
        input: definition.input.clone(),
    };
    Ok(RegisteredCommand {
        definition,
        descriptor,
    })
}

fn normalize_result(command: &str, result: CommandResult) -> anyhow::Result<CommandResult> {
    match &result {
        CommandResult::Success {
            source_event_seq: Some(sequence),
            ..
        } => anyhow::ensure!(
            *sequence <= MAX_SAFE_INTEGER,
            "command {command:?} success sourceEventSeq must be a non-negative safe integer when supplied"
        ),
        CommandResult::Error { text } => anyhow::ensure!(
            !text.trim().is_empty(),
            "command {command:?} error text must be a non-empty string"
        ),
        CommandResult::Success { .. } => {}
    }
    Ok(result)
}

fn append_run(
    session: &Session,
    command_id: &CommandId,
    parsed: &ParsedCommand,
    record_input: bool,
) -> anyhow::Result<()> {
    let mut data = Map::from_iter([
        (
            "commandId".to_owned(),
            Value::String(command_id.to_string()),
        ),
        ("name".to_owned(), Value::String(parsed.name.clone())),
        (
            "source".to_owned(),
            serde_json::to_value(CommandSource::user()).expect("command source is lossless JSON"),
        ),
    ]);
    if record_input {
        data.insert("args".to_owned(), Value::String(parsed.raw_input.clone()));
    }
    session.append("command/run", Value::Object(data), AppendOptions::default())?;
    Ok(())
}

fn append_done(
    session: &Session,
    command_id: &CommandId,
    result: &CommandResult,
) -> anyhow::Result<()> {
    let mut data = Map::from_iter([
        (
            "commandId".to_owned(),
            Value::String(command_id.to_string()),
        ),
        ("kind".to_owned(), Value::String(result.kind().to_owned())),
    ]);
    if let Some(text) = result.text() {
        data.insert("text".to_owned(), Value::String(text.to_owned()));
    }
    if let Some(sequence) = result.source_event_seq() {
        data.insert("sourceEventSeq".to_owned(), Value::from(sequence));
    }
    session.append(
        "command/done",
        Value::Object(data),
        AppendOptions::default(),
    )?;
    Ok(())
}

fn append_done_error(
    session: &Session,
    command_id: &CommandId,
    error: &anyhow::Error,
) -> anyhow::Result<()> {
    session.append(
        "command/done",
        json!({
            "commandId": command_id.as_str(),
            "kind": "error",
            "text": error.to_string()
        }),
        AppendOptions::default(),
    )?;
    Ok(())
}

fn abort_error(signal: &AbortSignal) -> anyhow::Error {
    let message = signal
        .reason()
        .and_then(|reason| reason.as_str().map(str::to_owned))
        .unwrap_or_else(|| "command aborted".to_owned());
    anyhow::anyhow!(message)
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("command handler panicked")
        .to_owned()
}

fn detach_handler(
    future: Pin<Box<dyn Future<Output = anyhow::Result<CommandResult>> + Send + 'static>>,
) {
    let task = async move {
        let _ = future.await;
    };
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(task);
    } else {
        std::thread::spawn(move || futures::executor::block_on(task));
    }
}

fn notify_change(context: &Context) {
    match context
        .events()
        .prepare_emit(context, "commands/change", &EventArgs::new())
    {
        Ok(emission) => emission.emit_contained(|error| {
            tracing::warn!(%error, "commands/change listener failed");
        }),
        Err(error) => tracing::warn!(%error, "commands/change dispatch preparation failed"),
    }
}

/// Installs and publishes the command runtime.
///
/// # Errors
///
/// Returns ordinary service registration failures.
pub fn install(context: &Context) -> anyhow::Result<Arc<CommandRuntime>> {
    let runtime = CommandRuntime::new(context);
    runtime.provide(context)?;
    register_invocable_service_if_available(context, COMMANDS)?;
    Ok(runtime)
}

/// Builds the Loader-compatible command registry plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(PLUGIN_NAME, PLUGIN_INJECT.iter().copied(), |context, _| {
        Box::pin(async move {
            install(&context)?;
            Ok(())
        })
    })
}

pub use invariant::register_invariant;
