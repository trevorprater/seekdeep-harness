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

mod format;

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

/// Custom printf-style placeholder formatter; `None` denotes an absent argument.
pub type LogFormatter =
    Arc<dyn Fn(Option<&Value>, &LogExporter, &LogMessage) -> String + Send + Sync + 'static>;

/// Structured log sink and its formatting/threshold options.
#[derive(Clone)]
pub struct LogExporter {
    /// ANSI color capability; zero disables colors.
    pub colors: u8,
    /// Maximum UTF-16 code-unit count per output line.
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
        format::format(exporter, message)
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

    /// Sets the limit applied after the next retained record; zero leaves the buffer unbounded.
    pub fn set_buffer_size(&self, size: usize) {
        self.buffer_size
            .store(u64::try_from(size).unwrap_or(u64::MAX), Ordering::Release);
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
        let service = self.clone();
        let effect = EffectHandle::synchronous("ctx.logger.exporter()", move || {
            let mut exporters = service.exporters.lock();
            exporters.remove(&service.exporter_sequence.load(Ordering::Acquire));
            Ok(())
        });
        let mut exporters = self.exporters.lock();
        let effect = owner.own(effect)?;
        let id = self.exporter_sequence.fetch_add(1, Ordering::AcqRel) + 1;
        exporters.insert(id, exporter);
        Ok(effect)
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
            .unwrap_or_else(|| seekdeep_cosmokit::string::param_case(context.fiber().name()));
        let level = config
            .and_then(|config| config.get("level"))
            .filter(|value| value.is_number())
            .map(format::number)
            .map(|value| {
                if value >= 3.0 {
                    3
                } else if value >= 2.0 {
                    2
                } else if value >= 1.0 {
                    1
                } else if value >= 0.0 {
                    0
                } else {
                    -1
                }
            });
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
        let mut previous = 0;
        loop {
            let next = self
                .exporters
                .lock()
                .range((
                    std::ops::Bound::Excluded(previous),
                    std::ops::Bound::Unbounded,
                ))
                .next()
                .map(|(serial, exporter)| (*serial, exporter.clone()));
            let Some((serial, exporter)) = next else {
                break;
            };
            previous = serial;
            if exporter.threshold(&message.name, fallback) >= level as i32 {
                exporter.export(message.clone());
            }
        }
    }
}

fn trim_buffer(buffer: &mut Vec<LogMessage>, size: usize) {
    if size != 0 && buffer.len() > size {
        buffer.drain(..buffer.len() - size);
    }
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
