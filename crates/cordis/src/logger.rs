//! Structured Cordis logging with source-compatible formatting and thresholds.

use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc, Weak,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use parking_lot::Mutex;
use serde_json::{Map, Value};

use crate::{Context, Fiber, fiber::EffectHandle};

/// Wall-clock boundary used for structured log timestamps.
pub trait CordisClock: std::fmt::Debug + Send + Sync {
    /// Current Unix time in milliseconds.
    fn now_ms(&self) -> i64;
}

/// Host wall-clock adapter used by [`Context::new`](crate::Context::new).
#[derive(Debug)]
pub struct SystemCordisClock;

impl CordisClock for SystemCordisClock {
    fn now_ms(&self) -> i64 {
        let milliseconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0_u128, |duration| duration.as_millis());
        i64::try_from(milliseconds).unwrap_or(i64::MAX)
    }
}

/// Logger method and severity category.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoggerType {
    /// Error.
    Error,
    /// Informational.
    Info,
    /// Warning.
    Warn,
    /// Debug.
    Debug,
}

impl LoggerType {
    /// Exact source spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Debug => "debug",
        }
    }
}

/// Numeric source severity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(i32)]
pub enum LoggerLevel {
    /// Error.
    Error = 0,
    /// Informational.
    Info = 1,
    /// Warning.
    Warn = 2,
    /// Debug.
    Debug = 3,
}

impl LoggerLevel {
    const fn logger_type(self) -> LoggerType {
        match self {
            Self::Error => LoggerType::Error,
            Self::Info => LoggerType::Info,
            Self::Warn => LoggerType::Warn,
            Self::Debug => LoggerType::Debug,
        }
    }
}

/// Structured record delivered to exporters.
#[derive(Clone, Debug)]
pub struct LogMessage {
    /// Monotonic message sequence.
    pub sn: u64,
    /// Injected wall-clock timestamp.
    pub ts: i64,
    /// Logger name.
    pub name: String,
    /// Severity name.
    pub message_type: LoggerType,
    /// Numeric severity.
    pub level: LoggerLevel,
    /// Original JSON-compatible arguments.
    pub args: Vec<Value>,
    /// Producing fiber, when it remains live.
    pub fiber: Weak<Fiber>,
    /// Logger-authored extra fields.
    pub meta: Map<String, Value>,
}

/// Custom printf-style placeholder formatter.
pub type LogFormatter =
    Arc<dyn Fn(&Value, &LogExporter, &LogMessage) -> String + Send + Sync + 'static>;

/// Structured log sink and its formatting/threshold options.
#[derive(Clone)]
pub struct LogExporter {
    /// ANSI color capability; zero disables colors.
    pub colors: u8,
    /// Maximum Unicode-scalar count per output line.
    pub max_length: usize,
    /// Per-name and `default` severity thresholds.
    pub levels: BTreeMap<String, i32>,
    /// Placeholder overrides.
    pub formatters: HashMap<char, LogFormatter>,
    callback: Arc<dyn Fn(LogMessage) + Send + Sync>,
}

impl std::fmt::Debug for LogExporter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LogExporter")
            .field("colors", &self.colors)
            .field("max_length", &self.max_length)
            .field("levels", &self.levels)
            .finish_non_exhaustive()
    }
}

impl LogExporter {
    /// Creates a sink with source defaults.
    #[must_use]
    pub fn new(callback: impl Fn(LogMessage) + Send + Sync + 'static) -> Self {
        Self {
            colors: 0,
            max_length: 10_240,
            levels: BTreeMap::new(),
            formatters: HashMap::new(),
            callback: Arc::new(callback),
        }
    }

    fn export(&self, message: LogMessage) {
        (self.callback)(message);
    }

    fn threshold(&self, name: &str, fallback: i32) -> i32 {
        self.levels
            .get(name)
            .or_else(|| self.levels.get("default"))
            .copied()
            .unwrap_or(fallback)
    }
}

/// Named logger construction options.
#[derive(Clone, Debug)]
pub struct LoggerOptions {
    /// Display name.
    pub name: String,
    /// Default exporter threshold.
    pub level: Option<i32>,
    /// Extra fields.
    pub meta: Map<String, Value>,
}

/// Named logger facade.
#[derive(Clone)]
pub struct Logger {
    options: LoggerOptions,
    service: Arc<LoggerService>,
    fiber: Weak<Fiber>,
}

impl std::fmt::Debug for Logger {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Logger")
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

impl Logger {
    /// Logger name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.options.name
    }

    /// Emits an error record.
    pub fn error(&self, args: impl IntoIterator<Item = Value>) {
        self.log(LoggerLevel::Error, args);
    }

    /// Emits an informational record.
    pub fn info(&self, args: impl IntoIterator<Item = Value>) {
        self.log(LoggerLevel::Info, args);
    }

    /// Emits a warning record.
    pub fn warn(&self, args: impl IntoIterator<Item = Value>) {
        self.log(LoggerLevel::Warn, args);
    }

    /// Emits a debug record.
    pub fn debug(&self, args: impl IntoIterator<Item = Value>) {
        self.log(LoggerLevel::Debug, args);
    }

    /// Emits one structured record.
    pub fn log(&self, level: LoggerLevel, args: impl IntoIterator<Item = Value>) {
        self.service.emit(
            &self.options,
            self.fiber.clone(),
            level,
            args.into_iter().collect(),
        );
    }

    /// Source-compatible logger-name color code.
    #[must_use]
    pub fn code(name: &str, colors: u8) -> Option<u16> {
        let palette: &[u16] = match colors {
            0 => return None,
            1 => &C16,
            _ => &C256,
        };
        let mut hash = 0_i32;
        for unit in name.encode_utf16() {
            hash = hash
                .wrapping_shl(3)
                .wrapping_sub(hash)
                .wrapping_add(i32::from(unit))
                .wrapping_add(13);
        }
        let index = usize::try_from(hash.unsigned_abs()).unwrap_or(0) % palette.len();
        Some(palette[index])
    }

    /// Applies source-compatible ANSI foreground coloring.
    #[must_use]
    pub fn color(exporter: &LogExporter, code: u16, value: &str, decoration: &str) -> String {
        if exporter.colors == 0 {
            return value.to_owned();
        }
        let code = if code < 8 {
            code.to_string()
        } else {
            format!("8;5;{code}")
        };
        let decoration = if exporter.colors >= 2 { decoration } else { "" };
        format!("\u{1b}[3{code}{decoration}m{value}\u{1b}[0m")
    }

    /// Formats a message through exporter overrides and source defaults.
    #[must_use]
    pub fn format(exporter: &LogExporter, message: &LogMessage) -> String {
        let mut args = message.args.clone();
        if args.first().is_none_or(|value| !value.is_string()) {
            args.insert(0, Value::String("%o".to_owned()));
        }
        let format = args
            .remove(0)
            .as_str()
            .map(str::to_owned)
            .unwrap_or_default();
        let mut values = args.into_iter();
        let mut output = String::new();
        let mut chars = format.chars().peekable();
        while let Some(character) = chars.next() {
            if character != '%' {
                output.push(character);
                continue;
            }
            let Some(code) = chars.next() else {
                output.push('%');
                break;
            };
            if code == '%' {
                output.push('%');
                continue;
            }
            let known = exporter.formatters.contains_key(&code)
                || matches!(code, 's' | 'd' | 'i' | 'f' | 'o' | 'O' | 'c' | 'C');
            if !known {
                output.push('%');
                output.push(code);
                continue;
            }
            let value = values.next().unwrap_or(Value::Null);
            if let Some(formatter) = exporter.formatters.get(&code) {
                output.push_str(&formatter(&value, exporter, message));
            } else {
                output.push_str(&default_format(code, &value, exporter, message));
            }
        }
        for value in values {
            output.push(' ');
            output.push_str(&append_value(&value));
        }
        output
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .map(|line| truncate_line(line, exporter.max_length))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Built-in logger service.
#[derive(Debug)]
pub struct LoggerService {
    clock: Arc<dyn CordisClock>,
    buffer_size: AtomicU64,
    buffer: Mutex<Vec<LogMessage>>,
    message_sequence: AtomicU64,
    exporter_sequence: AtomicU64,
    exporters: Mutex<BTreeMap<u64, LogExporter>>,
}

impl LoggerService {
    /// Creates a service with a deterministic clock seam.
    #[must_use]
    pub fn new(clock: Arc<dyn CordisClock>) -> Arc<Self> {
        Arc::new(Self {
            clock,
            buffer_size: AtomicU64::new(1_000),
            buffer: Mutex::new(Vec::new()),
            message_sequence: AtomicU64::new(0),
            exporter_sequence: AtomicU64::new(0),
            exporters: Mutex::new(BTreeMap::new()),
        })
    }

    /// Sets the retained record count.
    pub fn set_buffer_size(&self, size: usize) {
        self.buffer_size
            .store(u64::try_from(size).unwrap_or(u64::MAX), Ordering::Release);
        trim_buffer(&mut self.buffer.lock(), size);
    }

    /// Detached retained records.
    #[must_use]
    pub fn buffer(&self) -> Vec<LogMessage> {
        self.buffer.lock().clone()
    }

    /// Registers an exporter owned by one fiber.
    ///
    /// # Errors
    ///
    /// Returns an inactive-owner failure.
    pub fn exporter(
        self: &Arc<Self>,
        owner: &Context,
        exporter: LogExporter,
    ) -> Result<EffectHandle, crate::CordisError> {
        let id = self.exporter_sequence.fetch_add(1, Ordering::AcqRel) + 1;
        self.exporters.lock().insert(id, exporter);
        let service = self.clone();
        let effect = EffectHandle::synchronous("ctx.logger.exporter()", move || {
            service.exporters.lock().remove(&id);
            Ok(())
        });
        match owner.own(effect.clone()) {
            Ok(effect) => Ok(effect),
            Err(error) => {
                self.exporters.lock().remove(&id);
                Err(error)
            }
        }
    }

    /// Creates a named logger using context intercepts and fiber defaults.
    #[must_use]
    pub fn logger(self: &Arc<Self>, context: &Context, name: Option<&str>) -> Logger {
        let config = context
            .intercepted("logger")
            .unwrap_or_else(|| Value::Object(Map::new()));
        let config = config.as_object();
        let name = name
            .map(str::to_owned)
            .or_else(|| config.and_then(|config| config.get("name")?.as_str().map(str::to_owned)))
            .unwrap_or_else(|| hyphenate(context.fiber().name()));
        let level = config
            .and_then(|config| config.get("level"))
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok());
        Logger {
            options: LoggerOptions {
                name,
                level,
                meta: Map::new(),
            },
            service: self.clone(),
            fiber: Arc::downgrade(context.fiber()),
        }
    }

    fn emit(
        &self,
        options: &LoggerOptions,
        fiber: Weak<Fiber>,
        level: LoggerLevel,
        args: Vec<Value>,
    ) {
        let message = LogMessage {
            sn: self.message_sequence.fetch_add(1, Ordering::AcqRel) + 1,
            ts: self.clock.now_ms(),
            name: options.name.clone(),
            message_type: level.logger_type(),
            level,
            args,
            fiber,
            meta: options.meta.clone(),
        };
        let fallback = options.level.unwrap_or(LoggerLevel::Info as i32);
        if fallback >= level as i32 {
            let mut buffer = self.buffer.lock();
            buffer.push(message.clone());
            trim_buffer(
                &mut buffer,
                usize::try_from(self.buffer_size.load(Ordering::Acquire)).unwrap_or(usize::MAX),
            );
        }
        let exporters = self.exporters.lock().values().cloned().collect::<Vec<_>>();
        for exporter in exporters {
            if exporter.threshold(&message.name, fallback) >= level as i32 {
                exporter.export(message.clone());
            }
        }
    }
}

fn trim_buffer(buffer: &mut Vec<LogMessage>, size: usize) {
    if buffer.len() > size {
        buffer.drain(..buffer.len() - size);
    }
}

fn hyphenate(value: &str) -> String {
    let mut output = String::new();
    for (index, character) in value.chars().enumerate() {
        if character.is_uppercase() {
            if index > 0 {
                output.push('-');
            }
            output.extend(character.to_lowercase());
        } else {
            output.push(character);
        }
    }
    output
}

fn default_format(
    code: char,
    value: &Value,
    exporter: &LogExporter,
    message: &LogMessage,
) -> String {
    match code {
        's' => javascript_string(value),
        'd' | 'i' => number(value).trunc().to_string(),
        'f' => number(value).to_string(),
        'o' | 'O' => serde_json::to_string(value).unwrap_or_else(|_| "null".to_owned()),
        'c' => String::new(),
        'C' => Logger::code(&message.name, exporter.colors).map_or_else(
            || javascript_string(value),
            |code| Logger::color(exporter, code, &javascript_string(value), ""),
        ),
        _ => format!("%{code}"),
    }
}

fn number(value: &Value) -> f64 {
    match value {
        Value::Number(value) => value.as_f64().unwrap_or(f64::NAN),
        Value::Bool(value) => i32::from(*value).into(),
        Value::Null => 0.0,
        Value::String(value) => value.parse().unwrap_or(f64::NAN),
        Value::Array(_) | Value::Object(_) => f64::NAN,
    }
}

fn javascript_string(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Array(values) => values
            .iter()
            .map(javascript_string)
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_owned(),
    }
}

fn append_value(value: &Value) -> String {
    match value {
        Value::Array(_) | Value::Object(_) => {
            serde_json::to_string(value).unwrap_or_else(|_| "null".to_owned())
        }
        _ => javascript_string(value),
    }
}

fn truncate_line(line: &str, max_length: usize) -> String {
    let units = line.encode_utf16().collect::<Vec<_>>();
    if units.len() <= max_length {
        return line.to_owned();
    }
    format!("{}...", String::from_utf16_lossy(&units[..max_length]))
}

/// ANSI 16-color palette.
pub const C16: [u16; 6] = [6, 2, 3, 4, 5, 1];
/// ANSI 256-color palette.
pub const C256: [u16; 75] = [
    20, 21, 26, 27, 32, 33, 38, 39, 40, 41, 42, 43, 44, 45, 56, 57, 62, 63, 68, 69, 74, 75, 76, 77,
    78, 79, 80, 81, 92, 93, 98, 99, 112, 113, 129, 134, 135, 148, 149, 160, 161, 162, 163, 164,
    165, 166, 167, 168, 169, 170, 171, 172, 173, 178, 179, 184, 185, 196, 197, 198, 199, 200, 201,
    202, 203, 204, 205, 206, 207, 208, 209, 214, 215, 220, 221,
];
