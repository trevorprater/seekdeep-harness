//! Native and browser console exporters over the shared `Cordis` logger.

use std::{collections::BTreeMap, io::IsTerminal as _, sync::Arc};

use chrono::{DateTime, Local};
use parking_lot::Mutex;
use seekdeep_cordis::{
    Context, LogExporter, LogFormatter, LogMessage, Logger, LoggerType, Plugin, fiber::EffectHandle,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Cordis plugin name.
pub const NAME: &str = "logger-console";
/// Console logging has no required service dependency.
pub const INJECT: &[&str] = &[];

/// Label alignment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LabelAlign {
    /// Prefix then left-aligned label.
    #[default]
    Left,
    /// Right-aligned label then prefix.
    Right,
}

/// Formatting options for the logger-name label.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LabelStyle {
    /// Minimum label width in UTF-16 code units.
    pub width: Option<usize>,
    /// Spaces around the label/prefix boundary.
    pub margin: Option<usize>,
    /// Label alignment.
    pub align: Option<LabelAlign>,
}

/// Console exporter configuration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Terminal color support level; false and zero both disable color.
    pub colors: Option<ColorSetting>,
    /// Maximum rendered line length.
    pub max_length: Option<usize>,
    /// Per-logger severity thresholds.
    pub levels: Option<BTreeMap<String, i32>>,
    /// Whether to append elapsed time from the prior emitted record.
    pub show_diff: bool,
    /// Local-time template; empty disables the clock prefix.
    pub show_time: String,
    /// Optional label padding and alignment.
    pub label: Option<LabelStyle>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            colors: None,
            max_length: None,
            levels: None,
            show_diff: false,
            show_time: "yyyy-MM-dd hh:mm:ss ".to_owned(),
            label: None,
        }
    }
}

/// Boolean-or-level color setting from the source config.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ColorSetting {
    /// Explicit false.
    Disabled(bool),
    /// Supports-color level zero through three.
    Level(u8),
}

impl ColorSetting {
    fn level(self) -> u8 {
        match self {
            Self::Disabled(_) => 0,
            Self::Level(level) => level.min(3),
        }
    }
}

/// Console method selected by the browser entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleMethod {
    /// `console.error`.
    Error,
    /// `console.warn`.
    Warn,
    /// `console.log`.
    Log,
}

/// Sink payload shared by native, browser, and conformance adapters.
#[derive(Clone, Debug, PartialEq)]
pub enum ConsoleRecord {
    /// Fully rendered native line.
    Rendered(String),
    /// Browser method, prefix, and original arguments.
    Browser {
        /// Native console method.
        method: ConsoleMethod,
        /// Stable severity/name prefix.
        prefix: String,
        /// Original structural arguments.
        args: Vec<Value>,
    },
}

/// Console output function.
pub type ConsoleSink = Arc<dyn Fn(ConsoleRecord) + Send + Sync>;

/// Clock boundary used for time prefixes and initial diff state.
pub trait ConsoleClock: std::fmt::Debug + Send + Sync {
    /// Current local calendar time.
    fn now(&self) -> DateTime<Local>;
    /// Current epoch milliseconds.
    fn now_millis(&self) -> i64;
}

/// Native local clock.
#[derive(Debug, Default)]
pub struct SystemConsoleClock;

impl ConsoleClock for SystemConsoleClock {
    fn now(&self) -> DateTime<Local> {
        Local::now()
    }

    fn now_millis(&self) -> i64 {
        Local::now().timestamp_millis()
    }
}

struct Renderer {
    config: Config,
    clock: Arc<dyn ConsoleClock>,
    timestamp: Mutex<i64>,
}

impl Renderer {
    fn render(&self, exporter: &LogExporter, message: &LogMessage) -> String {
        let prefix = format!(
            "[{}]",
            &message.message_type.as_str()[..1].to_ascii_uppercase()
        );
        let margin = self
            .config
            .label
            .as_ref()
            .and_then(|label| label.margin)
            .unwrap_or(1);
        let space = " ".repeat(margin);
        let mut indent = 3 + space.len();
        let mut output = String::new();
        if !self.config.show_time.is_empty() {
            indent += self.config.show_time.encode_utf16().count();
            let time = seekdeep_cosmokit::time::template(&self.config.show_time, self.clock.now());
            output.push_str(&Logger::color(exporter, 8, &time, ""));
        }
        let code = Logger::code(&message.name, exporter.colors).unwrap_or(0);
        let label = Logger::color(exporter, code, &message.name, ";1");
        let width = self
            .config
            .label
            .as_ref()
            .and_then(|style| style.width)
            .unwrap_or(0);
        let pad_length = width
            + label
                .encode_utf16()
                .count()
                .saturating_sub(message.name.encode_utf16().count());
        if self.config.label.as_ref().and_then(|style| style.align) == Some(LabelAlign::Right) {
            output.push_str(&pad_start(&label, pad_length));
            output.push_str(&space);
            output.push_str(&prefix);
            output.push_str(&space);
            indent += width + space.len();
        } else {
            output.push_str(&prefix);
            output.push_str(&space);
            output.push_str(&pad_end(&label, pad_length));
            output.push_str(&space);
        }
        output.push_str(
            &Logger::format(exporter, message).replace('\n', &format!("\n{}", " ".repeat(indent))),
        );
        let mut timestamp = self.timestamp.lock();
        if self.config.show_diff && *timestamp != 0 {
            let diff = message.ts.saturating_sub(*timestamp);
            output.push_str(&Logger::color(
                exporter,
                code,
                &format!(" +{}", seekdeep_cosmokit::time::format(i64_as_f64(diff))),
                "",
            ));
        }
        *timestamp = message.ts;
        output
    }
}

/// Installed exporter and its exact reversible registration.
pub struct ConsoleInstallation {
    renderer: Arc<Renderer>,
    formatting: LogExporter,
    effect: EffectHandle,
}

impl std::fmt::Debug for ConsoleInstallation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConsoleInstallation")
            .finish_non_exhaustive()
    }
}

impl ConsoleInstallation {
    /// Renders one record with the installed native formatting state.
    #[must_use]
    pub fn render(&self, message: &LogMessage) -> String {
        self.renderer.render(&self.formatting, message)
    }

    /// Disposes the exact logger exporter.
    ///
    /// # Errors
    ///
    /// Returns a lifecycle disposal failure.
    pub async fn dispose(&self) -> anyhow::Result<()> {
        self.effect.dispose().await
    }
}

/// Installs a native rendered-line exporter with explicit boundaries.
///
/// # Errors
///
/// Returns invalid config or lifecycle ownership failures.
pub fn install_with_sink(
    context: &Context,
    config: &Config,
    clock: Arc<dyn ConsoleClock>,
    sink: ConsoleSink,
) -> anyhow::Result<ConsoleInstallation> {
    validate_config(config)?;
    let renderer = Arc::new(Renderer {
        timestamp: Mutex::new(clock.now_millis()),
        config: config.clone(),
        clock,
    });
    let formatting = formatting_exporter(config, true);
    let callback_renderer = Arc::clone(&renderer);
    let callback_formatting = formatting.clone();
    let mut exporter = formatting_exporter(config, true);
    exporter = replace_callback(exporter, move |message| {
        sink(ConsoleRecord::Rendered(
            callback_renderer.render(&callback_formatting, &message),
        ));
    });
    let effect = context.logger_service().exporter(context, exporter)?;
    Ok(ConsoleInstallation {
        renderer,
        formatting,
        effect,
    })
}

/// Installs the native stdout exporter.
///
/// # Errors
///
/// Returns the same failures as [`install_with_sink`].
pub fn install(context: &Context, config: &Config) -> anyhow::Result<ConsoleInstallation> {
    install_with_sink(
        context,
        config,
        Arc::new(SystemConsoleClock),
        Arc::new(|record| {
            if let ConsoleRecord::Rendered(line) = record {
                println!("{line}");
            }
        }),
    )
}

/// Builds the native Loader plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, config| {
        Box::pin(async move {
            let config = serde_json::from_value(config)?;
            install(&context, &config)?;
            Ok(())
        })
    })
    .with_config_validator(normalize_config)
}

fn validate_config(config: &Config) -> anyhow::Result<()> {
    if let Some(ColorSetting::Level(level)) = config.colors {
        anyhow::ensure!(
            level <= 3,
            "logger-console colors must be false or an integer from 0 to 3"
        );
    }
    if let Some(ColorSetting::Disabled(value)) = config.colors {
        anyhow::ensure!(
            !value,
            "logger-console colors must be false or an integer from 0 to 3"
        );
    }
    if let Some(max_length) = config.max_length {
        anyhow::ensure!(max_length > 0, "logger-console maxLength must be positive");
    }
    Ok(())
}

pub(crate) fn normalize_config(value: &Value) -> anyhow::Result<Value> {
    let config = if value.is_null() {
        Config::default()
    } else {
        serde_json::from_value(value.clone())?
    };
    validate_config(&config)?;
    Ok(serde_json::to_value(config)?)
}

fn formatting_exporter(config: &Config, node: bool) -> LogExporter {
    let mut exporter = LogExporter::new(|_| {});
    exporter.colors = config.colors.map_or_else(
        || if node { native_color_level() } else { 0 },
        ColorSetting::level,
    );
    if let Some(max_length) = config.max_length {
        exporter.max_length = max_length;
    }
    if let Some(levels) = &config.levels {
        exporter.levels = levels.clone();
    }
    if node {
        let inspect: LogFormatter =
            Arc::new(|value: &Value, _: &LogExporter, _: &LogMessage| inspect_value(value));
        exporter.formatters.insert('o', Arc::clone(&inspect));
        exporter.formatters.insert('O', inspect);
    }
    exporter
}

fn native_color_level() -> u8 {
    if std::env::var_os("NO_COLOR").is_some() {
        return 0;
    }
    if let Some(force) = std::env::var_os("FORCE_COLOR") {
        let force = force.to_string_lossy();
        return match force.as_ref() {
            "0" | "false" => 0,
            "2" => 2,
            "3" => 3,
            _ => 1,
        };
    }
    u8::from(std::io::stdout().is_terminal())
}

fn i64_as_f64(value: i64) -> f64 {
    value
        .to_string()
        .parse()
        .expect("every i64 has an f64 representation")
}

fn replace_callback(
    template: LogExporter,
    callback: impl Fn(LogMessage) + Send + Sync + 'static,
) -> LogExporter {
    let mut exporter = LogExporter::new(callback);
    exporter.colors = template.colors;
    exporter.max_length = template.max_length;
    exporter.levels = template.levels;
    exporter.formatters = template.formatters;
    exporter
}

fn pad_start(value: &str, width: usize) -> String {
    let length = value.encode_utf16().count();
    format!("{}{value}", " ".repeat(width.saturating_sub(length)))
}

fn pad_end(value: &str, width: usize) -> String {
    let length = value.encode_utf16().count();
    format!("{value}{}", " ".repeat(width.saturating_sub(length)))
}

fn inspect_value(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => inspect_string(value),
        Value::Array(values) if values.is_empty() => "[]".to_owned(),
        Value::Array(values) => format!(
            "[ {} ]",
            values
                .iter()
                .map(inspect_value)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Object(values) if values.is_empty() => "{}".to_owned(),
        Value::Object(values) => {
            let mut entries = values.iter().enumerate().collect::<Vec<_>>();
            entries.sort_by_key(|(position, (key, _))| {
                js_array_index(key).map_or((1_u8, 0_u32, *position), |index| (0, index, 0))
            });
            format!(
                "{{ {} }}",
                entries
                    .into_iter()
                    .map(|(_, (key, value))| {
                        format!("{}: {}", inspect_key(key), inspect_value(value))
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    }
}

fn js_array_index(value: &str) -> Option<u32> {
    if value.is_empty() || (value.starts_with('0') && value != "0") {
        return None;
    }
    let index = value.parse::<u32>().ok()?;
    (index < u32::MAX && index.to_string() == value).then_some(index)
}

fn inspect_key(value: &str) -> String {
    let mut characters = value.chars();
    let valid = characters
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic() || matches!(character, '_' | '$'))
        && characters
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '$'));
    if valid {
        value.to_owned()
    } else {
        inspect_string(value)
    }
}

fn inspect_string(value: &str) -> String {
    let mut output = String::from("'");
    for character in value.chars() {
        match character {
            '\\' => output.push_str("\\\\"),
            '\'' => output.push_str("\\'"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            '\u{0008}' => output.push_str("\\b"),
            '\u{000c}' => output.push_str("\\f"),
            character if character.is_control() && u32::from(character) <= 0xff => {
                use std::fmt::Write as _;
                write!(&mut output, "\\x{:02X}", u32::from(character))
                    .expect("writing to a String is infallible");
            }
            character if character.is_control() => {
                use std::fmt::Write as _;
                write!(&mut output, "\\u{{{:x}}}", u32::from(character))
                    .expect("writing to a String is infallible");
            }
            character => output.push(character),
        }
    }
    output.push('\'');
    output
}

fn browser_record(message: &LogMessage) -> ConsoleRecord {
    let method = match message.message_type {
        LoggerType::Error => ConsoleMethod::Error,
        LoggerType::Warn => ConsoleMethod::Warn,
        LoggerType::Info | LoggerType::Debug => ConsoleMethod::Log,
    };
    ConsoleRecord::Browser {
        method,
        prefix: format!(
            "[{}] {}",
            &message.message_type.as_str()[..1].to_ascii_uppercase(),
            message.name
        ),
        args: message.args.clone(),
    }
}

/// Installs browser-method dispatch with an explicit sink.
///
/// # Errors
///
/// Returns invalid config or lifecycle ownership failures.
pub fn install_browser_with_sink(
    context: &Context,
    config: &Config,
    sink: ConsoleSink,
) -> anyhow::Result<EffectHandle> {
    validate_config(config)?;
    let mut exporter = formatting_exporter(config, false);
    exporter = replace_callback(exporter, move |message| sink(browser_record(&message)));
    Ok(context.logger_service().exporter(context, exporter)?)
}

#[cfg(target_arch = "wasm32")]
mod browser;

#[cfg(target_arch = "wasm32")]
pub use browser::{browser_plugin, install_browser};
