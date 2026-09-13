//! Launcher argument grammar for the `seekdeep` binary.
//!
//! The launcher owns only profile selection, patch overlays, config dumps, and
//! plugin management. For profile boots, the first token outside that grammar
//! starts an immutable suffix owned by the booted application. This explicit
//! scanner intentionally does not use Clap: ordinary option parsing would not
//! preserve the source launcher's stop-at-first-unowned-token and subcommand
//! boundary semantics.

use std::error::Error;
use std::fmt;

/// Product name used by launcher help and diagnostics.
pub const LAUNCHER_NAME: &str = "seekdeep";

/// Product-renamed launcher description from the source command.
pub const LAUNCHER_DESCRIPTION: &str = "seekdeep: boot a SeekDeep Harness profile — an ordered stack of plugin-bundle patch layers under your own overrides.";

/// Help for the profile selector. Kept separate from rendering so the process
/// entry point can reproduce its terminal layout without moving parse policy
/// back into the renderer.
pub const PROFILE_OPTION_HELP: &str = "the profile under $SEEKDEEP_HOME/profiles to boot";

/// Product-renamed examples appended to launcher help.
pub const HELP_EXAMPLES: &str = r#"
Examples:
  seekdeep --profile web                          boot the web profile (same as: seekdeep web)
  seekdeep --profile headless "run the tests"     answer one task, print the result, and exit
  seekdeep --profile tui --patch ./extra.yml      boot a custom profile with one extra overlay
  seekdeep --profile tui --resume <session>       arguments after the launcher flags reach the app
  seekdeep --profile web --help                   the web app's own flags and help
  seekdeep plugin --profile tui add <package>     install a plugin into the tui profile

"#;

/// Complete non-terminal launcher help with the source Commander's 80-column
/// fallback wrapping.
///
/// Interactive output is rendered by [`launcher_help`] using the live stdout
/// width. Non-interactive output retains this fixed snapshot, including its two
/// trailing newlines.
pub const LAUNCHER_HELP: &str = r#"Usage: seekdeep [options] [command] [args...]

seekdeep: boot a SeekDeep Harness profile — an ordered stack of plugin-bundle
patch layers under your own overrides.

Arguments:
  args                        arguments for the booted profile's app (see:
                              seekdeep --profile <name> --help)

Options:
  -V, --version               output the version number
  --profile <name>            the profile under $SEEKDEEP_HOME/profiles to boot
  --patch <path>              extra patch-list overlay applied after the profile
                              layer (repeatable)
  --dump-config               print the composed profile tree and exit
  --dump-default-config       print the profile tree without its user layer or
                              --patch overlays and exit

Commands:
  web [options] [args...]     boot the web profile (alias of --profile web); the
                              web app's own flags follow
  plugin [options] [args...]  manage a profile's plugins by forwarding the
                              remaining arguments to pnpm in the profile
                              directory

Examples:
  seekdeep --profile web                          boot the web profile (same as: seekdeep web)
  seekdeep --profile headless "run the tests"     answer one task, print the result, and exit
  seekdeep --profile tui --patch ./extra.yml      boot a custom profile with one extra overlay
  seekdeep --profile tui --resume <session>       arguments after the launcher flags reach the app
  seekdeep --profile web --help                   the web app's own flags and help
  seekdeep plugin --profile tui add <package>     install a plugin into the tui profile

"#;

const HELP_MINIMUM_WRAP_WIDTH: usize = 40;
const HELP_ITEM_INDENT: usize = 2;
const HELP_ITEM_SPACER: usize = 2;

const HELP_ARGUMENTS: &[(&str, &str)] = &[(
    "args",
    "arguments for the booted profile's app (see: seekdeep --profile <name> --help)",
)];

const HELP_OPTIONS: &[(&str, &str)] = &[
    ("-V, --version", "output the version number"),
    (
        "--profile <name>",
        "the profile under $SEEKDEEP_HOME/profiles to boot",
    ),
    (
        "--patch <path>",
        "extra patch-list overlay applied after the profile layer (repeatable)",
    ),
    ("--dump-config", "print the composed profile tree and exit"),
    (
        "--dump-default-config",
        "print the profile tree without its user layer or --patch overlays and exit",
    ),
];

const HELP_COMMANDS: &[(&str, &str)] = &[
    (
        "web [options] [args...]",
        "boot the web profile (alias of --profile web); the web app's own flags follow",
    ),
    (
        "plugin [options] [args...]",
        "manage a profile's plugins by forwarding the remaining arguments to pnpm in the profile directory",
    ),
];

/// Render launcher help using stdout's terminal width or Commander's
/// 80-column fallback for non-terminal output.
#[must_use]
pub fn launcher_help() -> String {
    stdout_help_width().map_or_else(|| LAUNCHER_HELP.to_owned(), render_launcher_help_at_width)
}

#[cfg(any(unix, windows))]
fn stdout_help_width() -> Option<usize> {
    terminal_size::terminal_size_of(std::io::stdout())
        .map(|(terminal_size::Width(width), _)| usize::from(width))
}

#[cfg(not(any(unix, windows)))]
fn stdout_help_width() -> Option<usize> {
    None
}

fn render_launcher_help_at_width(width: usize) -> String {
    let term_width = HELP_ARGUMENTS
        .iter()
        .chain(HELP_OPTIONS)
        .chain(HELP_COMMANDS)
        .map(|(term, _)| display_width(term))
        .max()
        .unwrap_or(0);
    let mut lines = vec![
        "Usage: seekdeep [options] [command] [args...]".to_owned(),
        String::new(),
        box_wrap(LAUNCHER_DESCRIPTION, width),
        String::new(),
    ];
    append_help_items(&mut lines, "Arguments:", HELP_ARGUMENTS, term_width, width);
    append_help_items(&mut lines, "Options:", HELP_OPTIONS, term_width, width);
    append_help_items(&mut lines, "Commands:", HELP_COMMANDS, term_width, width);
    let mut rendered = lines.join("\n");
    rendered.push_str(HELP_EXAMPLES);
    rendered
}

fn append_help_items(
    output: &mut Vec<String>,
    heading: &str,
    items: &[(&str, &str)],
    term_width: usize,
    help_width: usize,
) {
    output.push(heading.to_owned());
    output.extend(
        items
            .iter()
            .map(|(term, description)| format_help_item(term, term_width, description, help_width)),
    );
    output.push(String::new());
}

fn format_help_item(term: &str, term_width: usize, description: &str, help_width: usize) -> String {
    let remaining_width = help_width.saturating_sub(
        term_width
            .saturating_add(HELP_ITEM_SPACER)
            .saturating_add(HELP_ITEM_INDENT),
    );
    let description = if remaining_width < HELP_MINIMUM_WRAP_WIDTH
        || description
            .split('\n')
            .skip(1)
            .any(|line| line.starts_with(char::is_whitespace))
    {
        description.to_owned()
    } else {
        box_wrap(description, remaining_width)
    };
    let first_indent = " ".repeat(HELP_ITEM_INDENT);
    let continuation_indent = " ".repeat(
        HELP_ITEM_INDENT
            .saturating_add(term_width)
            .saturating_add(HELP_ITEM_SPACER),
    );
    format!(
        "{first_indent}{term:<term_width$}{spacer}{description}",
        spacer = " ".repeat(HELP_ITEM_SPACER),
    )
    .replace('\n', &format!("\n{continuation_indent}"))
}

fn box_wrap(value: &str, width: usize) -> String {
    if width < HELP_MINIMUM_WRAP_WIDTH {
        return value.to_owned();
    }
    value
        .split(['\r', '\n'])
        .filter(|line| !line.is_empty() || value.contains('\n'))
        .map(|line| wrap_help_line(line, width))
        .collect::<Vec<_>>()
        .join("\n")
}

fn wrap_help_line(line: &str, width: usize) -> String {
    let mut words = line.split_whitespace();
    let Some(first) = words.next() else {
        return String::new();
    };
    let mut output = first.to_owned();
    let mut line_width = display_width(first);
    for word in words {
        let word_width = display_width(word);
        if line_width.saturating_add(1).saturating_add(word_width) <= width {
            output.push(' ');
            output.push_str(word);
            line_width = line_width.saturating_add(1).saturating_add(word_width);
        } else {
            output.push('\n');
            output.push_str(word);
            line_width = word_width;
        }
    }
    output
}

fn display_width(value: &str) -> usize {
    value.encode_utf16().count()
}

/// A profile identifier exactly as supplied on the command line.
///
/// The launcher rejects only the empty string. Existence, whitespace, path
/// characters, and profile initialization belong to the profile layer.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProfileName(String);

impl ProfileName {
    /// Validate and retain a profile name without normalization.
    ///
    /// # Errors
    ///
    /// Returns [`SeekDeepArgError::EmptyProfile`] for an empty value.
    pub fn new(value: impl Into<String>) -> Result<Self, SeekDeepArgError> {
        let value = value.into();
        if value.is_empty() {
            return Err(SeekDeepArgError::EmptyProfile);
        }
        Ok(Self(value))
    }

    /// Borrow the original profile name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Recover the original profile name.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl AsRef<str> for ProfileName {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for ProfileName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Which of the source launcher's three dispatch paths was selected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvocationMode {
    /// Boot a profile and hand its application the unparsed suffix.
    Profile,
    /// Compose and print a profile without booting it.
    DumpConfig,
    /// Run pnpm in a profile directory.
    Plugin,
}

/// Boot a named profile and hand its application an immutable argv suffix.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProfileInvocation {
    /// Profile selected by `--profile` or the `web` alias.
    pub profile: ProfileName,
    /// Extra patch overlays, in command-line order.
    pub patches: Vec<String>,
    /// Tokens owned by the booted application, retained verbatim.
    pub args: Vec<String>,
}

/// Print a composed profile tree without booting it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DumpConfigInvocation {
    /// Profile whose layers should be composed.
    pub profile: ProfileName,
    /// Whether to omit user and command-line patch layers.
    pub default_only: bool,
    /// Extra overlays for a full dump, in command-line order.
    pub patches: Vec<String>,
}

/// Forward pnpm arguments inside a profile directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginInvocation {
    /// Profile whose dependency set should be managed.
    pub profile: ProfileName,
    /// Non-launcher tokens, retained in their original order.
    pub args: Vec<String>,
}

/// A launcher invocation that reaches runtime dispatch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SeekDeepInvocation {
    /// Boot a named profile.
    Profile(ProfileInvocation),
    /// Print a composed profile tree.
    DumpConfig(DumpConfigInvocation),
    /// Manage a profile's plugins.
    Plugin(PluginInvocation),
}

impl SeekDeepInvocation {
    /// Return the closed dispatch mode for exhaustive callers.
    #[must_use]
    pub const fn mode(&self) -> InvocationMode {
        match self {
            Self::Profile(_) => InvocationMode::Profile,
            Self::DumpConfig(_) => InvocationMode::DumpConfig,
            Self::Plugin(_) => InvocationMode::Plugin,
        }
    }
}

/// A successful launcher-owned terminal action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LauncherExit {
    /// Render launcher help to stdout and exit zero.
    Help,
    /// Print the supplied binary version to stdout and exit zero.
    Version(String),
}

impl LauncherExit {
    /// Help and version are both successful terminal actions.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        0
    }
}

/// The result of parsing an argv tail.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseOutcome {
    /// Continue into exactly one runtime dispatch path.
    Invocation(SeekDeepInvocation),
    /// Print launcher-owned output and exit without booting.
    Exit(LauncherExit),
}

/// A launcher option that requires one following value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValueOption {
    /// `--profile <name>`.
    Profile,
    /// `--patch <path>`.
    Patch,
}

/// A launcher subcommand that refuses root boot options.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LauncherSubcommand {
    /// The `web` profile alias.
    Web,
    /// The pnpm forwarding command.
    Plugin,
}

impl fmt::Display for LauncherSubcommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Web => formatter.write_str("web"),
            Self::Plugin => formatter.write_str("plugin"),
        }
    }
}

/// A source-compatible launcher usage failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SeekDeepArgError {
    /// The root command had no profile and no launcher-help request.
    MissingProfile,
    /// The plugin command omitted its child `--profile` option.
    MissingPluginProfile,
    /// A single-valued option had no following token.
    MissingValue(ValueOption),
    /// A profile value was exactly the empty string.
    EmptyProfile,
    /// A patch value was exactly the empty string.
    EmptyPatch,
    /// Both dump modes were selected.
    ConflictingDumpModes,
    /// A boot-free dump was given application arguments.
    DumpHasAppArguments(Vec<String>),
    /// A bundle-only dump was given one or more patch overlays.
    DefaultDumpHasPatches,
    /// Root boot options appeared before a child command.
    ParentOptionsBeforeSubcommand(LauncherSubcommand),
    /// Plugin mode had no pnpm arguments after launcher options were removed.
    PluginNeedsArguments,
}

impl SeekDeepArgError {
    /// Source parse failures are usage errors.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        1
    }
}

impl fmt::Display for SeekDeepArgError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingProfile => formatter.write_str("error: --profile <name> is required"),
            Self::MissingPluginProfile => {
                formatter.write_str("error: required option '--profile <name>' not specified")
            }
            Self::MissingValue(ValueOption::Profile) => {
                formatter.write_str("error: option '--profile <name>' argument missing")
            }
            Self::MissingValue(ValueOption::Patch) => {
                formatter.write_str("error: option '--patch <path>' argument missing")
            }
            Self::EmptyProfile => formatter.write_str("error: --profile needs a name"),
            Self::EmptyPatch => formatter.write_str("error: --patch needs a path"),
            Self::ConflictingDumpModes => formatter
                .write_str("error: --dump-config and --dump-default-config are mutually exclusive"),
            Self::DumpHasAppArguments(args) => {
                formatter.write_str("error: config dumps take no app arguments, got ")?;
                for (index, argument) in args.iter().enumerate() {
                    if index > 0 {
                        formatter.write_str(" ")?;
                    }
                    formatter.write_str(&quote_json_string(argument))?;
                }
                Ok(())
            }
            Self::DefaultDumpHasPatches => formatter.write_str(
                "error: --dump-default-config prints the bundle layers and takes no --patch",
            ),
            Self::ParentOptionsBeforeSubcommand(command) => write!(
                formatter,
                "error: {command} takes none of parent --profile, --patch, --dump-config, or --dump-default-config"
            ),
            Self::PluginNeedsArguments => formatter
                .write_str("error: plugin needs pnpm arguments to forward (e.g. add <package>)"),
        }
    }
}

impl Error for SeekDeepArgError {}

#[derive(Default)]
struct RootOptions {
    profile: Option<String>,
    boot: BootOptions,
}

impl RootOptions {
    fn has_child_forbidden_options(&self) -> bool {
        self.profile.is_some()
            || !self.boot.patches.is_empty()
            || self.boot.dump_config
            || self.boot.dump_default_config
    }
}

#[derive(Default)]
struct BootOptions {
    patches: Vec<String>,
    dump_config: bool,
    dump_default_config: bool,
}

/// Parse launcher arguments after the binary name.
///
/// `version` is retained only when the root launcher owns `-V` or
/// `--version`. Help/version tokens past an application boundary remain in
/// the invocation's `args` unchanged.
///
/// # Errors
///
/// Returns a source-compatible usage error for an invalid launcher-owned
/// option combination or a missing required value.
pub fn parse_seekdeep_args(
    argv: &[String],
    version: &str,
) -> Result<ParseOutcome, SeekDeepArgError> {
    let mut root = RootOptions::default();
    let mut index = 0;

    while index < argv.len() {
        let argument = &argv[index];
        match argument.as_str() {
            "--" => {
                index += 1;
                return parse_after_root_delimiter(&argv[index..], root);
            }
            "web" => {
                return parse_web(&argv[index + 1..], root.has_child_forbidden_options(), true);
            }
            "plugin" => {
                return parse_plugin(&argv[index + 1..], root.has_child_forbidden_options(), true);
            }
            "--profile" => {
                root.profile = Some(take_value(argv, &mut index, ValueOption::Profile)?);
            }
            "--patch" => {
                root.boot
                    .patches
                    .push(take_value(argv, &mut index, ValueOption::Patch)?);
            }
            "--dump-config" => root.boot.dump_config = true,
            "--dump-default-config" => root.boot.dump_default_config = true,
            "-V" | "--version" => {
                return Ok(ParseOutcome::Exit(LauncherExit::Version(
                    version.to_owned(),
                )));
            }
            _ => {
                // Commander treats characters attached to its boolean short
                // version option as a short-option cluster and exits as soon
                // as it sees `-V` (for example, `-Vanything`).
                if argument.starts_with("-V") && !argument.starts_with("--") {
                    return Ok(ParseOutcome::Exit(LauncherExit::Version(
                        version.to_owned(),
                    )));
                } else if let Some(value) = argument.strip_prefix("--profile=") {
                    root.profile = Some(value.to_owned());
                } else if let Some(value) = argument.strip_prefix("--patch=") {
                    root.boot.patches.push(value.to_owned());
                } else {
                    return finish_root(root, argv[index..].to_vec());
                }
            }
        }
        index += 1;
    }

    finish_root(root, Vec::new())
}

fn take_value(
    argv: &[String],
    index: &mut usize,
    option: ValueOption,
) -> Result<String, SeekDeepArgError> {
    *index += 1;
    argv.get(*index)
        .cloned()
        .ok_or(SeekDeepArgError::MissingValue(option))
}

fn parse_after_root_delimiter(
    remaining: &[String],
    root: RootOptions,
) -> Result<ParseOutcome, SeekDeepArgError> {
    match remaining.split_first() {
        Some((command, args)) if command == "web" => {
            parse_web(args, root.has_child_forbidden_options(), false)
        }
        Some((command, args)) if command == "plugin" => {
            parse_plugin(args, root.has_child_forbidden_options(), false)
        }
        _ => finish_root(root, remaining.to_vec()),
    }
}

fn finish_root(root: RootOptions, args: Vec<String>) -> Result<ParseOutcome, SeekDeepArgError> {
    let Some(profile) = root.profile else {
        if args
            .iter()
            .any(|argument| argument == "-h" || argument == "--help")
        {
            return Ok(ParseOutcome::Exit(LauncherExit::Help));
        }
        return Err(SeekDeepArgError::MissingProfile);
    };
    let profile = ProfileName::new(profile)?;
    resolve_boot(profile, root.boot, args)
}

fn parse_web(
    argv: &[String],
    has_parent_options: bool,
    options_enabled: bool,
) -> Result<ParseOutcome, SeekDeepArgError> {
    let mut boot = BootOptions::default();
    let mut forwarded_args = Vec::new();

    if options_enabled {
        let mut index = 0;
        while index < argv.len() {
            let argument = &argv[index];
            match argument.as_str() {
                "--" => {
                    forwarded_args.extend_from_slice(&argv[index + 1..]);
                    break;
                }
                "--patch" => {
                    boot.patches
                        .push(take_value(argv, &mut index, ValueOption::Patch)?);
                }
                "--dump-config" => boot.dump_config = true,
                "--dump-default-config" => boot.dump_default_config = true,
                _ => {
                    if let Some(value) = argument.strip_prefix("--patch=") {
                        boot.patches.push(value.to_owned());
                    } else {
                        forwarded_args.extend_from_slice(&argv[index..]);
                        break;
                    }
                }
            }
            index += 1;
        }
    } else {
        forwarded_args.extend_from_slice(argv);
    }

    if has_parent_options {
        return Err(SeekDeepArgError::ParentOptionsBeforeSubcommand(
            LauncherSubcommand::Web,
        ));
    }
    resolve_boot(ProfileName("web".to_owned()), boot, forwarded_args)
}

fn parse_plugin(
    argv: &[String],
    has_parent_options: bool,
    options_enabled: bool,
) -> Result<ParseOutcome, SeekDeepArgError> {
    let mut profile = None;
    let mut forwarded_args = Vec::new();
    let mut saw_unknown_option = false;

    if options_enabled {
        let mut index = 0;
        while index < argv.len() {
            let argument = &argv[index];
            match argument.as_str() {
                "--" => {
                    if saw_unknown_option {
                        forwarded_args.push(argument.clone());
                    }
                    forwarded_args.extend_from_slice(&argv[index + 1..]);
                    break;
                }
                "--profile" => {
                    profile = Some(take_value(argv, &mut index, ValueOption::Profile)?);
                }
                _ => {
                    if let Some(value) = argument.strip_prefix("--profile=") {
                        profile = Some(value.to_owned());
                    } else {
                        saw_unknown_option |= is_plugin_unknown_option(argument);
                        forwarded_args.push(argument.clone());
                    }
                }
            }
            index += 1;
        }
    } else {
        forwarded_args.extend_from_slice(argv);
    }

    let profile = profile.ok_or(SeekDeepArgError::MissingPluginProfile)?;
    if has_parent_options {
        return Err(SeekDeepArgError::ParentOptionsBeforeSubcommand(
            LauncherSubcommand::Plugin,
        ));
    }
    let profile = ProfileName::new(profile)?;
    if forwarded_args.is_empty() {
        return Err(SeekDeepArgError::PluginNeedsArguments);
    }
    Ok(ParseOutcome::Invocation(SeekDeepInvocation::Plugin(
        PluginInvocation {
            profile,
            args: forwarded_args,
        },
    )))
}

fn is_plugin_unknown_option(argument: &str) -> bool {
    argument.len() > 1 && argument.starts_with('-') && !is_commander_negative_number(argument)
}

fn is_commander_negative_number(argument: &str) -> bool {
    let Some(number) = argument.strip_prefix('-') else {
        return false;
    };
    let (mantissa, exponent) = number
        .split_once('e')
        .map_or((number, None), |(mantissa, exponent)| {
            (mantissa, Some(exponent))
        });
    if exponent.is_some_and(|_| mantissa.contains('e')) {
        return false;
    }

    let mantissa_is_number = if let Some((integer, fraction)) = mantissa.split_once('.') {
        integer.bytes().all(|byte| byte.is_ascii_digit())
            && !fraction.is_empty()
            && fraction.bytes().all(|byte| byte.is_ascii_digit())
    } else {
        !mantissa.is_empty() && mantissa.bytes().all(|byte| byte.is_ascii_digit())
    };
    if !mantissa_is_number {
        return false;
    }

    exponent.is_none_or(|exponent| {
        let digits = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn resolve_boot(
    profile: ProfileName,
    boot: BootOptions,
    args: Vec<String>,
) -> Result<ParseOutcome, SeekDeepArgError> {
    if boot.patches.iter().any(String::is_empty) {
        return Err(SeekDeepArgError::EmptyPatch);
    }
    if !boot.dump_config && !boot.dump_default_config {
        return Ok(ParseOutcome::Invocation(SeekDeepInvocation::Profile(
            ProfileInvocation {
                profile,
                patches: boot.patches,
                args,
            },
        )));
    }
    if boot.dump_config && boot.dump_default_config {
        return Err(SeekDeepArgError::ConflictingDumpModes);
    }
    if !args.is_empty() {
        return Err(SeekDeepArgError::DumpHasAppArguments(args));
    }
    if boot.dump_default_config && !boot.patches.is_empty() {
        return Err(SeekDeepArgError::DefaultDumpHasPatches);
    }
    Ok(ParseOutcome::Invocation(SeekDeepInvocation::DumpConfig(
        DumpConfigInvocation {
            profile,
            default_only: boot.dump_default_config,
            patches: boot.patches,
        },
    )))
}

fn quote_json_string(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for character in value.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\u{0008}' => quoted.push_str("\\b"),
            '\u{000c}' => quoted.push_str("\\f"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            character if character <= '\u{001f}' => {
                use fmt::Write as _;
                write!(quoted, "\\u{:04x}", u32::from(character))
                    .expect("writing to a String is infallible");
            }
            character => quoted.push(character),
        }
    }
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;

    const VERSION: &str = "1.2.3";

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn parse(values: &[&str]) -> Result<ParseOutcome, SeekDeepArgError> {
        parse_seekdeep_args(&strings(values), VERSION)
    }

    fn named_profile(value: &str) -> Result<ProfileName, SeekDeepArgError> {
        ProfileName::new(value)
    }

    fn profile(
        name: &str,
        patches: &[&str],
        args: &[&str],
    ) -> Result<ParseOutcome, SeekDeepArgError> {
        Ok(ParseOutcome::Invocation(SeekDeepInvocation::Profile(
            ProfileInvocation {
                profile: named_profile(name)?,
                patches: strings(patches),
                args: strings(args),
            },
        )))
    }

    fn dump(
        name: &str,
        default_only: bool,
        patches: &[&str],
    ) -> Result<ParseOutcome, SeekDeepArgError> {
        Ok(ParseOutcome::Invocation(SeekDeepInvocation::DumpConfig(
            DumpConfigInvocation {
                profile: named_profile(name)?,
                default_only,
                patches: strings(patches),
            },
        )))
    }

    fn plugin(name: &str, args: &[&str]) -> Result<ParseOutcome, SeekDeepArgError> {
        Ok(ParseOutcome::Invocation(SeekDeepInvocation::Plugin(
            PluginInvocation {
                profile: named_profile(name)?,
                args: strings(args),
            },
        )))
    }

    #[test]
    fn routes_profile_boots_and_repeatable_patches() {
        assert_eq!(parse(&["--profile", "tui"]), profile("tui", &[], &[]));
        assert_eq!(
            parse(&["--profile", "tui", "--patch", "a.yml", "--patch", "b.yml",]),
            profile("tui", &["a.yml", "b.yml"], &[])
        );
        assert_eq!(
            parse(&["--profile=tui", "--patch=a.yml", "--patch=b.yml"]),
            profile("tui", &["a.yml", "b.yml"], &[])
        );
    }

    #[test]
    fn root_stops_at_first_unowned_token() {
        assert_eq!(
            parse(&["--profile", "tui", "--resume", "abc"]),
            profile("tui", &[], &["--resume", "abc"])
        );
        assert_eq!(
            parse(&[
                "--profile",
                "tui",
                "--patch",
                "a.yml",
                "--resume",
                "b",
                "--patch",
                "late.yml",
            ]),
            profile("tui", &["a.yml"], &["--resume", "b", "--patch", "late.yml"],)
        );
        assert_eq!(
            parse(&["--profile", "headless", "run", "the", "tests"]),
            profile("headless", &[], &["run", "the", "tests"])
        );
    }

    #[test]
    fn root_value_options_consume_exactly_one_token_even_when_it_looks_owned() {
        assert_eq!(
            parse(&["--profile", "--dump-config"]),
            profile("--dump-config", &[], &[])
        );
        assert_eq!(
            parse(&["--profile", "x", "--patch", "--resume"]),
            profile("x", &["--resume"], &[])
        );
        assert_eq!(
            parse(&["--profile", "x", "--patch", "web"]),
            profile("x", &["web"], &[])
        );
    }

    #[test]
    fn web_is_a_profile_alias_with_its_own_boundary() {
        assert_eq!(parse(&["web"]), profile("web", &[], &[]));
        assert_eq!(
            parse(&["web", "--patch", "web.yml"]),
            profile("web", &["web.yml"], &[])
        );
        assert_eq!(
            parse(&["web", "--host", "127.0.0.1", "--port", "8080", "--dev",]),
            profile(
                "web",
                &[],
                &["--host", "127.0.0.1", "--port", "8080", "--dev"],
            )
        );
        assert_eq!(
            parse(&["web", "--profile", "x", "--patch", "late.yml"]),
            profile("web", &[], &["--profile", "x", "--patch", "late.yml"])
        );
    }

    #[test]
    fn plugin_removes_profiles_anywhere_before_the_delimiter() {
        assert_eq!(
            parse(&["plugin", "--profile", "tui", "add", "turtle-ui"]),
            plugin("tui", &["add", "turtle-ui"])
        );
        assert_eq!(
            parse(&["plugin", "--save-dev", "--profile", "tui", "add", "x",]),
            plugin("tui", &["--save-dev", "add", "x"])
        );
        assert_eq!(
            parse(&["plugin", "add", "--profile", "tui", "x"]),
            plugin("tui", &["add", "x"])
        );
        assert_eq!(
            parse(&["plugin", "--profile", "first", "add", "--profile=last", "x",]),
            plugin("last", &["add", "x"])
        );
    }

    #[test]
    fn plugin_forwards_help_version_and_unknown_pnpm_flags() {
        assert_eq!(
            parse(&["plugin", "--profile", "tui", "add", "--save-dev", "x",]),
            plugin("tui", &["add", "--save-dev", "x"])
        );
        assert_eq!(
            parse(&["plugin", "--profile", "tui", "--help"]),
            plugin("tui", &["--help"])
        );
        assert_eq!(
            parse(&["plugin", "--profile", "tui", "--version"]),
            plugin("tui", &["--version"])
        );
        assert_eq!(
            parse(&["plugin", "--profile", "tui", "--save-dev", "--", "--foo",]),
            plugin("tui", &["--save-dev", "--", "--foo"])
        );
        assert_eq!(
            parse(&["plugin", "--profile", "tui", "add", "--", "--foo"]),
            plugin("tui", &["add", "--foo"])
        );
        assert_eq!(
            parse(&["plugin", "--profile", "tui", "-1", "--", "--foo"]),
            plugin("tui", &["-1", "--foo"])
        );
    }

    #[test]
    fn routes_profile_and_web_config_dumps() {
        assert_eq!(
            parse(&["--profile", "web", "--dump-config"]),
            dump("web", false, &[])
        );
        assert_eq!(
            parse(&["--profile", "web", "--dump-default-config"]),
            dump("web", true, &[])
        );
        assert_eq!(
            parse(&["--profile", "tui", "--dump-config", "--patch", "x.yml",]),
            dump("tui", false, &["x.yml"])
        );
        assert_eq!(parse(&["web", "--dump-config"]), dump("web", false, &[]));
        assert_eq!(
            parse(&["web", "--dump-default-config"]),
            dump("web", true, &[])
        );
    }

    #[test]
    fn rejects_missing_profile_removed_forms_and_contradictions() {
        let cases = [
            (vec![], SeekDeepArgError::MissingProfile),
            (vec!["tui"], SeekDeepArgError::MissingProfile),
            (vec!["--config", "c.yml"], SeekDeepArgError::MissingProfile),
            (vec!["-p", "task"], SeekDeepArgError::MissingProfile),
            (vec!["run", "task"], SeekDeepArgError::MissingProfile),
            (vec!["--dump-config"], SeekDeepArgError::MissingProfile),
            (vec!["--bogus"], SeekDeepArgError::MissingProfile),
        ];
        for (argv, expected) in cases {
            assert_eq!(parse(&argv), Err(expected), "argv: {argv:?}");
        }

        assert_eq!(
            parse(&["--profile", "x", "--dump-config", "--dump-default-config"]),
            Err(SeekDeepArgError::ConflictingDumpModes)
        );
        assert_eq!(
            parse(&[
                "--profile",
                "x",
                "--dump-default-config",
                "--patch",
                "p.yml",
            ]),
            Err(SeekDeepArgError::DefaultDumpHasPatches)
        );
        assert_eq!(
            parse(&["--profile", "x", "--dump-config", "task"]),
            Err(SeekDeepArgError::DumpHasAppArguments(strings(&["task"])))
        );
        assert_eq!(
            parse(&["web", "--dump-config", "--dump-default-config"]),
            Err(SeekDeepArgError::ConflictingDumpModes)
        );
        assert_eq!(
            parse(&["web", "--dump-default-config", "--patch", "w.yml",]),
            Err(SeekDeepArgError::DefaultDumpHasPatches)
        );
        assert_eq!(
            parse(&["web", "--dump-config", "--port", "8080"]),
            Err(SeekDeepArgError::DumpHasAppArguments(strings(&[
                "--port", "8080",
            ])))
        );
        assert_eq!(
            parse(&["--profile", "web", "--dump-config", "-h"]),
            Err(SeekDeepArgError::DumpHasAppArguments(strings(&["-h"])))
        );
    }

    #[test]
    fn rejects_empty_values_but_preserves_other_profile_text() {
        assert_eq!(
            parse(&["--profile", ""]),
            Err(SeekDeepArgError::EmptyProfile)
        );
        assert_eq!(
            parse(&["--profile", "x", "--patch="]),
            Err(SeekDeepArgError::EmptyPatch)
        );
        assert_eq!(
            parse(&["web", "--patch="]),
            Err(SeekDeepArgError::EmptyPatch)
        );
        assert_eq!(
            parse(&["plugin", "--profile", "", "add"]),
            Err(SeekDeepArgError::EmptyProfile)
        );
        assert_eq!(parse(&["--profile", "  "]), profile("  ", &[], &[]));
    }

    #[test]
    fn rejects_parent_options_before_child_commands() {
        assert_eq!(
            parse(&["--profile", "x", "web"]),
            Err(SeekDeepArgError::ParentOptionsBeforeSubcommand(
                LauncherSubcommand::Web,
            ))
        );
        assert_eq!(
            parse(&["--patch", "x.yml", "plugin", "--profile", "tui", "add", "y",]),
            Err(SeekDeepArgError::ParentOptionsBeforeSubcommand(
                LauncherSubcommand::Plugin,
            ))
        );
    }

    #[test]
    fn plugin_required_option_and_argument_errors_match_source_precedence() {
        assert_eq!(
            parse(&["plugin", "add", "x"]),
            Err(SeekDeepArgError::MissingPluginProfile)
        );
        assert_eq!(
            parse(&["plugin", "--profile", "tui"]),
            Err(SeekDeepArgError::PluginNeedsArguments)
        );
        assert_eq!(
            parse(&["--profile", "parent", "plugin", "add"]),
            Err(SeekDeepArgError::MissingPluginProfile)
        );
        assert_eq!(
            parse(&["--profile", "parent", "plugin", "--profile", "child", "add",]),
            Err(SeekDeepArgError::ParentOptionsBeforeSubcommand(
                LauncherSubcommand::Plugin,
            ))
        );
    }

    #[test]
    fn missing_single_values_are_parse_errors() {
        assert_eq!(
            parse(&["--profile"]),
            Err(SeekDeepArgError::MissingValue(ValueOption::Profile))
        );
        assert_eq!(
            parse(&["--profile", "x", "--patch"]),
            Err(SeekDeepArgError::MissingValue(ValueOption::Patch))
        );
        assert_eq!(
            parse(&["web", "--patch"]),
            Err(SeekDeepArgError::MissingValue(ValueOption::Patch))
        );
        assert_eq!(
            parse(&["plugin", "add", "--profile"]),
            Err(SeekDeepArgError::MissingValue(ValueOption::Profile))
        );
    }

    #[test]
    fn root_help_is_owned_only_when_there_is_no_profile() {
        assert_eq!(
            parse(&["--help"]),
            Ok(ParseOutcome::Exit(LauncherExit::Help))
        );
        assert_eq!(parse(&["-h"]), Ok(ParseOutcome::Exit(LauncherExit::Help)));
        assert_eq!(
            parse(&["--bogus", "--help"]),
            Ok(ParseOutcome::Exit(LauncherExit::Help))
        );
        assert_eq!(
            parse(&["--profile", "web", "-h"]),
            profile("web", &[], &["-h"])
        );
        assert_eq!(parse(&["web", "--help"]), profile("web", &[], &["--help"]));
    }

    #[test]
    fn launcher_version_is_owned_only_before_the_root_boundary() {
        let version = Ok(ParseOutcome::Exit(LauncherExit::Version(
            VERSION.to_owned(),
        )));
        assert_eq!(parse(&["--version"]), version);
        assert_eq!(parse(&["-V"]), version);
        assert_eq!(parse(&["-Vanything"]), version);
        assert_eq!(parse(&["--profile", "x", "--version"]), version);
        assert_eq!(
            parse(&["--profile", "x", "task", "--version"]),
            profile("x", &[], &["task", "--version"])
        );
        assert_eq!(
            parse(&["web", "--version"]),
            profile("web", &[], &["--version"])
        );
    }

    #[test]
    fn delimiter_is_consumed_but_a_second_delimiter_is_forwarded() {
        assert_eq!(
            parse(&["--profile", "x", "--", "--patch", "late.yml"]),
            profile("x", &[], &["--patch", "late.yml"])
        );
        assert_eq!(
            parse(&["--profile", "x", "--", "--", "web"]),
            profile("x", &[], &["--", "web"])
        );
        assert_eq!(
            parse(&["web", "--", "--patch", "late.yml"]),
            profile("web", &[], &["--patch", "late.yml"])
        );
        assert_eq!(
            parse(&["plugin", "--profile", "tui", "--", "--profile", "other",]),
            plugin("tui", &["--profile", "other"])
        );
    }

    #[test]
    fn exact_first_app_argument_still_selects_a_subcommand_after_delimiter() {
        assert_eq!(
            parse(&["--", "web", "--patch", "late.yml"]),
            profile("web", &[], &["--patch", "late.yml"])
        );
        assert_eq!(
            parse(&["--profile", "x", "--", "web"]),
            Err(SeekDeepArgError::ParentOptionsBeforeSubcommand(
                LauncherSubcommand::Web,
            ))
        );
        assert_eq!(
            parse(&["--", "plugin", "--profile", "tui", "add"]),
            Err(SeekDeepArgError::MissingPluginProfile)
        );
    }

    #[test]
    fn dump_validation_order_matches_resolve_boot() {
        assert_eq!(
            parse(&[
                "--profile",
                "x",
                "--patch=",
                "--dump-config",
                "--dump-default-config",
                "task",
            ]),
            Err(SeekDeepArgError::EmptyPatch)
        );
        assert_eq!(
            parse(&[
                "--profile",
                "x",
                "--dump-config",
                "--dump-default-config",
                "task",
            ]),
            Err(SeekDeepArgError::ConflictingDumpModes)
        );
        assert_eq!(
            parse(&[
                "--profile",
                "x",
                "--dump-default-config",
                "--patch",
                "p.yml",
                "task",
            ]),
            Err(SeekDeepArgError::DumpHasAppArguments(strings(&["task"])))
        );
    }

    #[test]
    fn error_diagnostics_and_exit_codes_match_the_source_contract() {
        let cases = [
            (
                SeekDeepArgError::MissingProfile,
                "error: --profile <name> is required",
            ),
            (
                SeekDeepArgError::MissingPluginProfile,
                "error: required option '--profile <name>' not specified",
            ),
            (
                SeekDeepArgError::MissingValue(ValueOption::Profile),
                "error: option '--profile <name>' argument missing",
            ),
            (
                SeekDeepArgError::MissingValue(ValueOption::Patch),
                "error: option '--patch <path>' argument missing",
            ),
            (
                SeekDeepArgError::EmptyProfile,
                "error: --profile needs a name",
            ),
            (SeekDeepArgError::EmptyPatch, "error: --patch needs a path"),
            (
                SeekDeepArgError::ConflictingDumpModes,
                "error: --dump-config and --dump-default-config are mutually exclusive",
            ),
            (
                SeekDeepArgError::DefaultDumpHasPatches,
                "error: --dump-default-config prints the bundle layers and takes no --patch",
            ),
            (
                SeekDeepArgError::PluginNeedsArguments,
                "error: plugin needs pnpm arguments to forward (e.g. add <package>)",
            ),
        ];
        for (error, message) in cases {
            assert_eq!(error.to_string(), message);
            assert_eq!(error.exit_code(), 1);
        }
        assert_eq!(LauncherExit::Help.exit_code(), 0);
        assert_eq!(LauncherExit::Version(VERSION.to_owned()).exit_code(), 0);
    }

    #[test]
    fn dump_argument_diagnostic_uses_json_string_escaping() {
        let error = SeekDeepArgError::DumpHasAppArguments(strings(&[
            "plain",
            "a\"b",
            "slash\\path",
            "line\nnext",
            "\u{0001}",
            "中文",
        ]));
        assert_eq!(
            error.to_string(),
            "error: config dumps take no app arguments, got \"plain\" \"a\\\"b\" \"slash\\\\path\" \"line\\nnext\" \"\\u0001\" \"中文\""
        );
    }

    #[test]
    fn modes_are_closed_and_exhaustive() {
        let outcomes = [
            parse(&["--profile", "tui"]).expect("profile parse"),
            parse(&["--profile", "tui", "--dump-config"]).expect("dump parse"),
            parse(&["plugin", "--profile", "tui", "why", "x"]).expect("plugin parse"),
        ];
        let modes: Vec<_> = outcomes
            .iter()
            .map(|outcome| match outcome {
                ParseOutcome::Invocation(invocation) => invocation.mode(),
                ParseOutcome::Exit(_) => panic!("expected invocation"),
            })
            .collect();
        assert_eq!(
            modes,
            [
                InvocationMode::Profile,
                InvocationMode::DumpConfig,
                InvocationMode::Plugin,
            ]
        );
    }

    #[test]
    fn help_identity_is_renamed_without_rewriting_opaque_arguments() {
        assert_eq!(LAUNCHER_NAME, "seekdeep");
        assert!(LAUNCHER_DESCRIPTION.contains("SeekDeep Harness"));
        assert!(!LAUNCHER_DESCRIPTION.contains("DeepSeek Harness"));
        assert!(PROFILE_OPTION_HELP.contains("$SEEKDEEP_HOME"));
        assert!(!HELP_EXAMPLES.contains("  dsh "));
        assert_eq!(
            parse(&["plugin", "--profile", "tui", "why", "@deepseek-ai/cordis",]),
            plugin("tui", &["why", "@deepseek-ai/cordis"])
        );
    }

    #[test]
    fn launcher_help_matches_the_product_renamed_commander_snapshot() {
        let expected = r#"Usage: seekdeep [options] [command] [args...]

seekdeep: boot a SeekDeep Harness profile — an ordered stack of plugin-bundle
patch layers under your own overrides.

Arguments:
  args                        arguments for the booted profile's app (see:
                              seekdeep --profile <name> --help)

Options:
  -V, --version               output the version number
  --profile <name>            the profile under $SEEKDEEP_HOME/profiles to boot
  --patch <path>              extra patch-list overlay applied after the profile
                              layer (repeatable)
  --dump-config               print the composed profile tree and exit
  --dump-default-config       print the profile tree without its user layer or
                              --patch overlays and exit

Commands:
  web [options] [args...]     boot the web profile (alias of --profile web); the
                              web app's own flags follow
  plugin [options] [args...]  manage a profile's plugins by forwarding the
                              remaining arguments to pnpm in the profile
                              directory

Examples:
  seekdeep --profile web                          boot the web profile (same as: seekdeep web)
  seekdeep --profile headless "run the tests"     answer one task, print the result, and exit
  seekdeep --profile tui --patch ./extra.yml      boot a custom profile with one extra overlay
  seekdeep --profile tui --resume <session>       arguments after the launcher flags reach the app
  seekdeep --profile web --help                   the web app's own flags and help
  seekdeep plugin --profile tui add <package>     install a plugin into the tui profile

"#;

        let actual = launcher_help();
        assert_eq!(actual, expected);
        assert_eq!(render_launcher_help_at_width(80), expected);
        assert!(actual.ends_with("\n\n"));
    }

    #[test]
    fn launcher_help_wraps_at_the_live_commander_terminal_width() {
        let narrow = r#"Usage: seekdeep [options] [command] [args...]

seekdeep: boot a SeekDeep Harness profile — an ordered stack
of plugin-bundle patch layers under your own overrides.

Arguments:
  args                        arguments for the booted profile's app (see: seekdeep --profile <name> --help)

Options:
  -V, --version               output the version number
  --profile <name>            the profile under $SEEKDEEP_HOME/profiles to boot
  --patch <path>              extra patch-list overlay applied after the profile layer (repeatable)
  --dump-config               print the composed profile tree and exit
  --dump-default-config       print the profile tree without its user layer or --patch overlays and exit

Commands:
  web [options] [args...]     boot the web profile (alias of --profile web); the web app's own flags follow
  plugin [options] [args...]  manage a profile's plugins by forwarding the remaining arguments to pnpm in the profile directory

Examples:
  seekdeep --profile web                          boot the web profile (same as: seekdeep web)
  seekdeep --profile headless "run the tests"     answer one task, print the result, and exit
  seekdeep --profile tui --patch ./extra.yml      boot a custom profile with one extra overlay
  seekdeep --profile tui --resume <session>       arguments after the launcher flags reach the app
  seekdeep --profile web --help                   the web app's own flags and help
  seekdeep plugin --profile tui add <package>     install a plugin into the tui profile

"#;
        let wide = r#"Usage: seekdeep [options] [command] [args...]

seekdeep: boot a SeekDeep Harness profile — an ordered stack of plugin-bundle patch layers under your own overrides.

Arguments:
  args                        arguments for the booted profile's app (see: seekdeep --profile <name> --help)

Options:
  -V, --version               output the version number
  --profile <name>            the profile under $SEEKDEEP_HOME/profiles to boot
  --patch <path>              extra patch-list overlay applied after the profile layer (repeatable)
  --dump-config               print the composed profile tree and exit
  --dump-default-config       print the profile tree without its user layer or --patch overlays and exit

Commands:
  web [options] [args...]     boot the web profile (alias of --profile web); the web app's own flags follow
  plugin [options] [args...]  manage a profile's plugins by forwarding the remaining arguments to pnpm in the profile
                              directory

Examples:
  seekdeep --profile web                          boot the web profile (same as: seekdeep web)
  seekdeep --profile headless "run the tests"     answer one task, print the result, and exit
  seekdeep --profile tui --patch ./extra.yml      boot a custom profile with one extra overlay
  seekdeep --profile tui --resume <session>       arguments after the launcher flags reach the app
  seekdeep --profile web --help                   the web app's own flags and help
  seekdeep plugin --profile tui add <package>     install a plugin into the tui profile

"#;

        assert_eq!(render_launcher_help_at_width(60), narrow);
        assert_eq!(render_launcher_help_at_width(120), wide);
    }
}
