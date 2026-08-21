//! Headless profile command-line parsing.

/// Stable service name used by the source composition.
pub const HEADLESS_STARTUP_SERVICE: &str = "headlessStartup";

/// Parsed task published by the headless startup provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeadlessStartupValues {
    /// Exact task text after joining positional words with one ASCII space.
    pub task: String,
}

/// Terminal or runnable result of parsing the headless profile's arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeadlessStartupAction {
    /// A non-whitespace task is ready for the runner.
    Run(HeadlessStartupValues),
    /// Parsing terminated the invocation through the launcher exit hook.
    Exit {
        /// Requested process status.
        code: i32,
        /// Text written to standard output.
        stdout: String,
        /// Text written to standard error.
        stderr: String,
    },
}

/// Parses the immutable inner argument snapshot owned by the headless profile.
///
/// Launcher flags have already ended before this function runs. The remaining
/// positional words are joined exactly as Commander's variadic positional does
/// in the source implementation.
#[must_use]
pub fn parse_headless_args(args: &[String]) -> HeadlessStartupAction {
    // Commander treats the first `--` as an option terminator and removes it
    // from the variadic positional. An exact help flag before that boundary
    // wins over unknown options anywhere in the option-parsed prefix.
    let option_boundary = args
        .iter()
        .position(|argument| argument == "--")
        .unwrap_or(args.len());
    let option_parsed_args = &args[..option_boundary];

    if option_parsed_args
        .iter()
        .any(|argument| argument == "-h" || argument == "--help")
    {
        return HeadlessStartupAction::Exit {
            code: 0,
            stdout: help_text(),
            stderr: String::new(),
        };
    }

    if let Some(option) = option_parsed_args
        .iter()
        .find(|argument| is_unknown_option(argument))
    {
        return HeadlessStartupAction::Exit {
            code: 1,
            stdout: String::new(),
            stderr: unknown_option_error(option),
        };
    }

    let task = args
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != option_boundary)
        .map(|(_, argument)| argument.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    if task.trim().is_empty() {
        return HeadlessStartupAction::Exit {
            code: 1,
            stdout: String::new(),
            stderr: concat!(
                "error: a task is required, for example: seekdeep --profile headless ",
                "\"run the tests\"\n"
            )
            .to_owned(),
        };
    }
    HeadlessStartupAction::Run(HeadlessStartupValues { task })
}

fn help_text() -> String {
    concat!(
        "Usage: seekdeep --profile headless [options] [task...]\n\n",
        "Answer one task, print the final assistant message, and exit.\n\n",
        "Arguments:\n",
        "  task        the task text; multiple words are joined by spaces\n\n",
        "Options:\n",
        "  -h, --help  show this help\n\n",
        "Examples:\n",
        "  seekdeep --profile headless \"run the tests\"     answer one task and exit\n\n"
    )
    .to_owned()
}

fn is_unknown_option(argument: &str) -> bool {
    argument.len() > 1 && argument.starts_with('-') && !is_negative_number(argument)
}

// Commander's leaf-command option parser accepts negative numbers as ordinary
// positional arguments when no digit is registered as a short option.
fn is_negative_number(argument: &str) -> bool {
    let Some(number) = argument.strip_prefix('-') else {
        return false;
    };
    let (mantissa, exponent) = number
        .split_once('e')
        .map_or((number, None), |(mantissa, exponent)| {
            (mantissa, Some(exponent))
        });

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

fn unknown_option_error(option: &str) -> String {
    let suggestion = option
        .strip_prefix("--")
        .filter(|candidate| is_similar(candidate, "help"))
        .map_or(String::new(), |_| "\n(Did you mean --help?)".to_owned());
    format!("error: unknown option '{option}'{suggestion}\n")
}

// Commander 15 uses optimal-string-alignment distance with a maximum distance
// of three and compares JavaScript UTF-16 code units. There is only one long
// option in this command, so this is the exact `--help` suggestion predicate.
fn is_similar(word: &str, candidate: &str) -> bool {
    const MAX_DISTANCE: usize = 3;
    let word: Vec<u16> = word.encode_utf16().collect();
    let candidate: Vec<u16> = candidate.encode_utf16().collect();
    let distance = optimal_string_alignment_distance(&word, &candidate, MAX_DISTANCE);
    let length = word.len().max(candidate.len());
    let matching_units = length.saturating_sub(distance);
    distance <= MAX_DISTANCE && length != 0 && (matching_units as u128) * 5 > (length as u128) * 2
}

fn optimal_string_alignment_distance(a: &[u16], b: &[u16], max_distance: usize) -> usize {
    if a.len().abs_diff(b.len()) > max_distance {
        return a.len().max(b.len());
    }

    let mut distances = vec![vec![0; b.len() + 1]; a.len() + 1];
    for (index, row) in distances.iter_mut().enumerate() {
        row[0] = index;
    }
    for (index, distance) in distances[0].iter_mut().enumerate() {
        *distance = index;
    }

    for j in 1..=b.len() {
        for i in 1..=a.len() {
            let substitution_cost = usize::from(a[i - 1] != b[j - 1]);
            distances[i][j] = (distances[i - 1][j] + 1)
                .min(distances[i][j - 1] + 1)
                .min(distances[i - 1][j - 1] + substitution_cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                distances[i][j] = distances[i][j].min(distances[i - 2][j - 2] + 1);
            }
        }
    }
    distances[a.len()][b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn joins_the_variadic_task_without_trimming_it() {
        assert_eq!(
            parse_headless_args(&strings(&["run", "the", "tests"])),
            HeadlessStartupAction::Run(HeadlessStartupValues {
                task: "run the tests".to_owned(),
            })
        );
        assert_eq!(
            parse_headless_args(&strings(&["  keep", "spacing  "])),
            HeadlessStartupAction::Run(HeadlessStartupValues {
                task: "  keep spacing  ".to_owned(),
            })
        );
    }

    #[test]
    fn missing_and_whitespace_only_tasks_are_usage_errors() {
        for args in [Vec::new(), strings(&["   "])] {
            let HeadlessStartupAction::Exit {
                code,
                stdout,
                stderr,
            } = parse_headless_args(&args)
            else {
                panic!("missing task unexpectedly ran");
            };
            assert_eq!(code, 1);
            assert!(stdout.is_empty());
            assert!(stderr.contains("a task is required"));
        }
    }

    #[test]
    fn help_is_owned_by_the_headless_application() {
        let HeadlessStartupAction::Exit {
            code,
            stdout,
            stderr,
        } = parse_headless_args(&strings(&["--help"]))
        else {
            panic!("help unexpectedly ran");
        };
        assert_eq!(code, 0);
        assert_eq!(
            stdout,
            concat!(
                "Usage: seekdeep --profile headless [options] [task...]\n\n",
                "Answer one task, print the final assistant message, and exit.\n\n",
                "Arguments:\n",
                "  task        the task text; multiple words are joined by spaces\n\n",
                "Options:\n",
                "  -h, --help  show this help\n\n",
                "Examples:\n",
                "  seekdeep --profile headless \"run the tests\"     answer one task and exit\n\n"
            )
        );
        assert!(stderr.is_empty());
    }

    #[test]
    fn option_delimiter_is_removed_and_makes_later_options_positional() {
        assert_eq!(
            parse_headless_args(&strings(&["--", "--help", "--bad"])),
            HeadlessStartupAction::Run(HeadlessStartupValues {
                task: "--help --bad".to_owned(),
            })
        );
        assert_eq!(
            parse_headless_args(&strings(&["first", "--", "--", "last"])),
            HeadlessStartupAction::Run(HeadlessStartupValues {
                task: "first -- last".to_owned(),
            })
        );

        let HeadlessStartupAction::Exit { code, stderr, .. } =
            parse_headless_args(&strings(&["--"]))
        else {
            panic!("an empty delimited task unexpectedly ran");
        };
        assert_eq!(code, 1);
        assert_eq!(
            stderr,
            "error: a task is required, for example: seekdeep --profile headless \"run the tests\"\n"
        );
    }

    #[test]
    fn help_before_the_delimiter_wins_but_help_after_it_is_task_text() {
        let HeadlessStartupAction::Exit {
            code,
            stdout,
            stderr,
        } = parse_headless_args(&strings(&["--bad", "--help"]))
        else {
            panic!("help unexpectedly ran the task");
        };
        assert_eq!(code, 0);
        assert!(stdout.ends_with("\n\n"));
        assert!(stderr.is_empty());

        assert_eq!(
            parse_headless_args(&strings(&["--", "--help"])),
            HeadlessStartupAction::Run(HeadlessStartupValues {
                task: "--help".to_owned(),
            })
        );
    }

    #[test]
    fn commander_negative_number_and_unknown_option_rules_are_preserved() {
        assert_eq!(
            parse_headless_args(&strings(&["-1", "-.5", "-1.25e-3"])),
            HeadlessStartupAction::Run(HeadlessStartupValues {
                task: "-1 -.5 -1.25e-3".to_owned(),
            })
        );

        for (option, expected_stderr) in [
            ("-1E3", "error: unknown option '-1E3'\n"),
            (
                "--help=x",
                "error: unknown option '--help=x'\n(Did you mean --help?)\n",
            ),
            (
                "--helpfoo",
                "error: unknown option '--helpfoo'\n(Did you mean --help?)\n",
            ),
        ] {
            let HeadlessStartupAction::Exit {
                code,
                stdout,
                stderr,
            } = parse_headless_args(&strings(&[option]))
            else {
                panic!("unknown option unexpectedly ran");
            };
            assert_eq!(code, 1);
            assert!(stdout.is_empty());
            assert_eq!(stderr, expected_stderr);
        }
    }
}
