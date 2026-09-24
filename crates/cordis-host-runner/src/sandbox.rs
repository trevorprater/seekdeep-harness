//! Side-effect-free validation for model-authored Host JavaScript bodies.

use boa_engine::{Source, context::ContextBuilder, script::Script};
use thiserror::Error;

/// One Host closure symbol exposed to model-authored packages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostBuiltinInspection {
    /// Exact symbol or Context root.
    pub name: &'static str,
    /// Capability and restriction summary.
    pub description: &'static str,
    /// Callable signatures exposed to Inspect.
    pub signatures: &'static [&'static str],
}

/// Exact Host Builtin directory returned by the Inspect provider.
pub const HOST_BUILTIN_INSPECTION: &[HostBuiltinInspection] = &[
    HostBuiltinInspection {
        name: "ctx",
        description: "Restricted Cordis Context. Prefer ctx.get(name) with an undefined check; use inject for hard dependencies.",
        signatures: &[
            "ctx.get(name: string): unknown | undefined",
            "ctx.on(name: string, listener: Function): () => void",
            "ctx.provide(name: string, value: unknown): () => void",
            "ctx.effect(callback: Function, label?: string): () => void",
        ],
    },
    HostBuiltinInspection {
        name: "harness",
        description: "Host helpers for Package-private Client RPC and model-visible dynamic Tools.",
        signatures: &[
            "harness.handle(method: string, handler: (args: JsonValue) => JsonValue | Promise<JsonValue>): () => void",
            "harness.defineTool(definition: ToolDefinition): ToolDefinition",
            "harness.registerTool(ctx: Context, tool: ToolDefinition): () => void",
        ],
    },
    HostBuiltinInspection {
        name: "console",
        description: "Package-tagged Host logging.",
        signatures: &[
            "console.log(...values): void",
            "console.error(...values): void",
        ],
    },
    HostBuiltinInspection {
        name: "btoa",
        description: "Encode UTF-8 text as base64.",
        signatures: &["btoa(value: string): string"],
    },
    HostBuiltinInspection {
        name: "atob",
        description: "Decode base64 as UTF-8 text.",
        signatures: &["atob(value: string): string"],
    },
    HostBuiltinInspection {
        name: "TextEncoder",
        description: "Standard UTF-8 encoder constructor.",
        signatures: &["new TextEncoder()"],
    },
    HostBuiltinInspection {
        name: "TextDecoder",
        description: "Standard text decoder constructor.",
        signatures: &["new TextDecoder(label?: string)"],
    },
];

/// Define argument whose JavaScript body is being validated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SandboxCodeHalf {
    /// `code.host` body.
    Host,
    /// `code.client` body.
    Client,
}

impl SandboxCodeHalf {
    const fn field(self) -> &'static str {
        match self {
            Self::Host => "code.host",
            Self::Client => "code.client",
        }
    }
}

/// Define-time JavaScript syntax failure with bounded teaching context.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{message}{context}{hint}")]
pub struct SandboxSyntaxError {
    /// Parser diagnostic.
    pub message: String,
    context: String,
    hint: String,
}

impl SandboxSyntaxError {
    /// Rendered source context, empty when no useful line can be identified.
    #[must_use]
    pub fn context(&self) -> &str {
        &self.context
    }

    /// Corrective teaching hint.
    #[must_use]
    pub fn hint(&self) -> &str {
        &self.hint
    }
}

/// Parses one async-function body without evaluating it or mutating registry state.
///
/// # Errors
///
/// Returns a bounded syntax diagnostic with plain-JavaScript and bracket hints.
pub fn validate_host_code(body: &str) -> Result<(), SandboxSyntaxError> {
    validate_code(body, SandboxCodeHalf::Host)
}

/// Parses one Host or Client async-function body without evaluating it.
///
/// # Errors
///
/// Returns a bounded syntax diagnostic labeled with the exact define field.
pub fn validate_code(body: &str, half: SandboxCodeHalf) -> Result<(), SandboxSyntaxError> {
    let source = format!("(async function __seekdeep_dynamic_host__() {{\n{body}\n}})");
    let mut javascript = ContextBuilder::new()
        .build()
        .map_err(|error| SandboxSyntaxError {
            message: error.to_string(),
            context: String::new(),
            hint: String::new(),
        })?;
    Script::parse(Source::from_bytes(&source), None, &mut javascript)
        .map(|_| ())
        .map_err(|error| syntax_error(body, half, &error.to_string()))
}

fn syntax_error(body: &str, half: SandboxCodeHalf, message: &str) -> SandboxSyntaxError {
    let line_count = body.lines().count();
    let line = parser_line(message).and_then(|line| {
        let line = line.saturating_sub(2).clamp(1, line_count.max(1));
        body.lines()
            .nth(line.saturating_sub(1))
            .map(|source| (line, source))
    });
    let context = line.as_ref().map_or_else(String::new, |(line, source)| {
        let bounded = source.chars().take(240).collect::<String>();
        format!("\n{line:>4} | {bounded}\n     | ^")
    });
    let offending_line = line.map_or("", |(_, source)| source);
    let typescript = offending_line
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .any(|word| word == "as");
    let hint = if typescript {
        "\nThe sandbox runs plain JavaScript, not TypeScript. Remove type annotations:\n  ✗ { type: 'text' as const, text: x }\n  ✓ { type: 'text', text: x }".to_owned()
    } else {
        let balance = if brackets_unbalanced(body) {
            " Check bracket balance — ending the returned plugin object with `});` closes a call that was never opened; a plain `return { … }` ends with `}` (an optional `;`), never `)`."
        } else {
            ""
        };
        format!(
            "\nNote: it runs as the BODY of an async function (line numbers are offset by the 1-line wrapper).{balance}"
        )
    };
    SandboxSyntaxError {
        message: format!(
            "dynamic package `{}` failed to parse:\n{message}",
            half.field()
        ),
        context,
        hint,
    }
}

/// Returns a `SyntaxError` stack prefix or its stable stackless fallback.
#[must_use]
pub fn syntax_error_context(message: &str, stack: Option<&str>) -> String {
    let lines = stack.unwrap_or_default().lines().collect::<Vec<_>>();
    let Some(index) = lines
        .iter()
        .position(|line| line.starts_with("SyntaxError"))
    else {
        return format!("SyntaxError: {message}");
    };
    lines[..=index].join("\n")
}

fn parser_line(message: &str) -> Option<usize> {
    for marker in ["line ", "line:", " at "] {
        let Some((_, tail)) = message.rsplit_once(marker) else {
            continue;
        };
        let digits = tail
            .trim_start()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>();
        if let Ok(line) = digits.parse() {
            return Some(line);
        }
    }
    None
}

fn brackets_unbalanced(source: &str) -> bool {
    let mut stack = Vec::new();
    for character in source.chars() {
        match character {
            '(' | '[' | '{' => stack.push(character),
            ')' if stack.pop() != Some('(') => return true,
            ']' if stack.pop() != Some('[') => return true,
            '}' if stack.pop() != Some('{') => return true,
            _ => {}
        }
    }
    !stack.is_empty()
}
