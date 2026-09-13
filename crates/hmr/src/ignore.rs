//! The default picomatch grammar used by Host HMR's `ignored` array.
//!
//! Adapted from picomatch 4.0.4; see `licenses/picomatch.txt`.

use regress::Regex;

const STAR: &str = "[^/]*?";
const GLOBSTAR: &str = r"(?:(?:(?!(?:^|\/)\.).)*?)";
const ONE: &str = "(?=.)";
const NO_DOT: &str = r"(?!\.)";
const NO_DOT_SLASH: &str = r"(?!\.{0,1}(?:\/|$))";
const SLASH: &str = r"\/";

/// Compiled source-compatible HMR ignore patterns.
#[derive(Debug)]
pub struct IgnoreMatcher {
    patterns: Vec<(String, Regex)>,
}

impl IgnoreMatcher {
    /// Compiles the source's default picomatch grammar.
    ///
    /// # Errors
    ///
    /// Rejects empty patterns and patterns exceeding 65,536 UTF-16 code units.
    pub fn new(patterns: &[String]) -> anyhow::Result<Self> {
        let mut compiled = Vec::with_capacity(patterns.len());
        for pattern in patterns {
            anyhow::ensure!(
                !pattern.is_empty(),
                "Expected pattern to be a non-empty string"
            );
            let length = pattern.encode_utf16().count();
            anyhow::ensure!(
                length <= 65_536,
                "Input length: {length}, exceeds maximum allowed length: 65536"
            );
            let source = compile(pattern);
            // Picomatch turns malformed regular expressions into a never-match
            // expression unless its explicitly requested debug option is set.
            let regex = Regex::from_unicode(
                source.encode_utf16().map(u32::from),
                regress::Flags::default(),
            )
            .or_else(|_| Regex::new("$^"))?;
            compiled.push((pattern.clone(), regex));
        }
        Ok(Self { patterns: compiled })
    }

    /// Matches any pattern, including the source's literal-equality shortcut.
    #[must_use]
    pub fn is_match(&self, path: &str) -> bool {
        !path.is_empty()
            && self.patterns.iter().any(|(pattern, regex)| {
                pattern == path
                    || regex
                        .find_from_ucs2(&path.encode_utf16().collect::<Vec<_>>(), 0)
                        .next()
                        .is_some()
            })
    }
}

fn replacement(pattern: &str) -> &str {
    match pattern {
        "***" => "*",
        "**/**" | "**/**/**" => "**",
        _ => pattern,
    }
}

fn compile(pattern: &str) -> String {
    let output = if (pattern.starts_with('.') || pattern.starts_with('*'))
        && let Some(output) = fast_path(
            replacement(pattern)
                .strip_prefix("./")
                .unwrap_or(replacement(pattern)),
        ) {
        output + r"\/?"
    } else {
        let parsed = Parser::new(replacement(pattern)).parse(true);
        let source = format!("^(?:{})$", parsed.output);
        return if parsed.negated {
            format!("^(?!{source}).*$")
        } else {
            source
        };
    };
    format!("^(?:{output})$")
}

fn fast_path(pattern: &str) -> Option<String> {
    Some(match pattern {
        "*" => format!("{NO_DOT}{ONE}{STAR}"),
        ".*" => format!(r"\.{ONE}{STAR}"),
        "*.*" => format!(r"{NO_DOT}{STAR}\.{ONE}{STAR}"),
        "*/*" => format!("{NO_DOT}{STAR}{SLASH}{ONE}{NO_DOT}{STAR}"),
        "**" => format!("{NO_DOT}{GLOBSTAR}"),
        "**/*" => format!("(?:{NO_DOT}{GLOBSTAR}{SLASH})?{NO_DOT}{ONE}{STAR}"),
        "**/*.*" => format!(r"(?:{NO_DOT}{GLOBSTAR}{SLASH})?{NO_DOT}{STAR}\.{ONE}{STAR}"),
        "**/.*" => format!(r"(?:{NO_DOT}{GLOBSTAR}{SLASH})?\.{ONE}{STAR}"),
        _ => {
            let (prefix, suffix) = pattern.rsplit_once('.')?;
            if suffix.is_empty() || !suffix.chars().all(is_word) {
                return None;
            }
            format!(r"{}\.{suffix}", fast_path(prefix)?)
        }
    })
}

fn is_word(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

fn regex_special(character: char) -> bool {
    "-*+?.^${}(|)[]".contains(character)
}

fn escape_regex(value: &str) -> String {
    let mut output = String::new();
    for character in value.chars() {
        if regex_special(character) {
            output.push('\\');
        }
        output.push(character);
    }
    output
}

fn escape_last(output: &mut String, character: char) {
    if let Some(index) = output
        .match_indices(character)
        .rev()
        .map(|(index, _)| index)
        .find(|index| *index == 0 || output.as_bytes()[index - 1] != b'\\')
    {
        output.insert(index, '\\');
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Bos,
    Text,
    Star,
    Globstar,
    Slash,
    Dot,
    Dots,
    Bracket,
    Brace,
    Paren,
    Comma,
    Qmark,
    Plus,
    At,
    Negate,
}

#[derive(Debug)]
struct Token {
    kind: Kind,
    value: String,
    output: String,
    prev: usize,
    star: bool,
    posix: bool,
    extglob: bool,
}

impl Token {
    fn new(kind: Kind, value: impl Into<String>, output: impl Into<String>) -> Self {
        Self {
            kind,
            value: value.into(),
            output: output.into(),
            prev: 0,
            star: false,
            posix: false,
            extglob: false,
        }
    }

    fn literal(kind: Kind, value: impl Into<String>) -> Self {
        let value = value.into();
        Self::new(kind, value.clone(), value)
    }
}

#[derive(Debug)]
struct Extglob {
    kind: Kind,
    close: String,
    inner: String,
    parens: usize,
    output: String,
    start: usize,
    token_start: usize,
}

#[derive(Debug)]
struct Brace {
    token: usize,
    output_start: usize,
    dots: bool,
    comma: bool,
}

struct Parsed {
    output: String,
    negated: bool,
}

struct Parser {
    input: Vec<char>,
    index: usize,
    start: usize,
    output: String,
    tokens: Vec<Token>,
    brackets: usize,
    parens: usize,
    quote: bool,
    braces: Vec<Brace>,
    stack: Vec<Kind>,
    extglobs: Vec<Extglob>,
    negated: bool,
    backtrack: bool,
}

impl Parser {
    fn new(pattern: &str) -> Self {
        Self {
            input: pattern
                .strip_prefix("./")
                .unwrap_or(pattern)
                .chars()
                .collect(),
            index: 0,
            start: 0,
            output: String::new(),
            tokens: vec![Token::literal(Kind::Bos, "")],
            brackets: 0,
            parens: 0,
            quote: false,
            braces: Vec::new(),
            stack: Vec::new(),
            extglobs: Vec::new(),
            negated: false,
            backtrack: false,
        }
    }

    fn peek(&self, offset: usize) -> Option<char> {
        self.input.get(self.index + offset).copied()
    }

    fn remaining(&self) -> String {
        self.input[self.index..].iter().collect()
    }

    fn previous(&self) -> &Token {
        self.tokens
            .last()
            .expect("the beginning token always exists")
    }

    fn previous_mut(&mut self) -> &mut Token {
        self.tokens
            .last_mut()
            .expect("the beginning token always exists")
    }

    fn push(&mut self, mut token: Token) {
        if self.previous().kind == Kind::Globstar {
            let brace = !self.braces.is_empty() && matches!(token.kind, Kind::Comma | Kind::Brace);
            let extglob = token.extglob || (!self.extglobs.is_empty() && token.kind == Kind::Paren);
            if !matches!(token.kind, Kind::Slash | Kind::Paren) && !brace && !extglob {
                self.output.truncate(
                    self.output
                        .len()
                        .saturating_sub(self.previous().output.len()),
                );
                let previous = self.previous_mut();
                previous.kind = Kind::Star;
                "*".clone_into(&mut previous.value);
                STAR.clone_into(&mut previous.output);
                self.output.push_str(STAR);
            }
        }
        if token.kind != Kind::Paren
            && let Some(extglob) = self.extglobs.last_mut()
        {
            extglob.inner.push_str(&token.value);
        }
        self.output.push_str(&token.output);
        if self.previous().kind == Kind::Text && token.kind == Kind::Text {
            let previous = self.previous_mut();
            if previous.output.is_empty() {
                previous.output.clone_from(&previous.value);
            }
            previous.output.push_str(&token.value);
            previous.value.push_str(&token.value);
            return;
        }
        token.prev = self.tokens.len() - 1;
        self.tokens.push(token);
    }

    fn open_extglob(&mut self, kind: Kind, value: char) {
        let close = match kind {
            Kind::Negate => format!(")){STAR})"),
            Kind::Qmark => ")?".to_owned(),
            Kind::Plus => ")+".to_owned(),
            Kind::Star => ")*".to_owned(),
            _ => ")".to_owned(),
        };
        let token = Extglob {
            kind,
            close,
            inner: String::new(),
            parens: self.parens,
            output: self.output.clone(),
            start: self.index - 1,
            token_start: self.tokens.len(),
        };
        self.parens += 1;
        self.stack.push(Kind::Paren);
        self.push(Token::new(
            kind,
            value.to_string(),
            if self.output.is_empty() { ONE } else { "" },
        ));
        self.index += 1;
        let mut opening = Token::new(
            Kind::Paren,
            "(",
            if kind == Kind::Negate {
                "(?:(?!(?:"
            } else {
                "(?:"
            },
        );
        opening.extglob = true;
        self.push(opening);
        self.extglobs.push(token);
    }

    fn close_extglob(&mut self, token: Extglob) {
        let body: String = self.input[token.start + 2..self.index - 1].iter().collect();
        if matches!(token.kind, Kind::Plus | Kind::Star)
            && let Some(safe) = repeated_extglob(&body)
        {
            let literal: String = self.input[token.start..self.index].iter().collect();
            let output = safe.map_or_else(
                || escape_regex(&literal),
                |safe| format!("{}{safe}", if token.output.is_empty() { ONE } else { "" }),
            );
            let opening = &mut self.tokens[token.token_start];
            opening.kind = Kind::Text;
            opening.value = literal;
            opening.output.clone_from(&output);
            for hidden in &mut self.tokens[token.token_start + 1..] {
                hidden.value.clear();
                hidden.output.clear();
            }
            self.output = token.output + output.as_str();
            self.backtrack = true;
            let mut close = Token::new(Kind::Paren, ")", "");
            close.extglob = true;
            self.push(close);
            self.parens = self.parens.saturating_sub(1);
            self.stack.pop();
            return;
        }
        let mut output = token.close;
        if token.kind == Kind::Negate {
            let star = if token.inner.len() > 1 && token.inner.contains('/') {
                GLOBSTAR
            } else {
                STAR
            };
            let rest = self.remaining();
            if star != STAR || rest.is_empty() || rest.chars().all(|value| value == ')') {
                output = format!(")$)){star}");
            }
            if token.inner.contains('*')
                && rest.starts_with('.')
                && rest.len() > 1
                && !rest[1..]
                    .chars()
                    .any(|character| matches!(character, '\\' | '/' | '.'))
            {
                output = format!("){}){star})", Self::new(&rest).parse(false).output);
            }
        }
        let mut close = Token::new(Kind::Paren, ")", output);
        close.extglob = true;
        self.push(close);
        self.parens = self.parens.saturating_sub(1);
        self.stack.pop();
    }

    #[expect(
        clippy::too_many_lines,
        reason = "One token dispatch updates the shared cursor, delimiter stack, and previous token."
    )]
    fn parse(mut self, use_fast: bool) -> Parsed {
        if use_fast
            && !self
                .input
                .first()
                .is_some_and(|value| matches!(value, '*' | '!'))
            && !self.input.iter().any(|value| "/()[]{}\"".contains(*value))
        {
            return Parsed {
                output: format!("^(?:{})$", simple_fast_path(&self.input)),
                negated: false,
            };
        }
        while let Some(character) = self.peek(0) {
            self.index += 1;
            if character == '\0' {
                continue;
            }
            let mut value = character.to_string();
            if character == '\\' {
                if self
                    .peek(0)
                    .is_some_and(|next| matches!(next, '/' | '.' | ';'))
                {
                    continue;
                }
                if self.peek(0).is_none() {
                    self.push(Token::literal(Kind::Text, "\\\\"));
                    continue;
                }
                let slashes = self.input[self.index..]
                    .iter()
                    .take_while(|next| **next == '\\')
                    .count();
                if slashes > 2 {
                    self.index += slashes;
                    if slashes % 2 != 0 {
                        value.push('\\');
                    }
                }
                if let Some(next) = self.peek(0) {
                    value.push(next);
                    self.index += 1;
                }
                if self.brackets == 0 {
                    self.push(Token::literal(Kind::Text, value));
                    continue;
                }
            }
            if self.brackets > 0
                && (character != ']' || matches!(self.previous().value.as_str(), "[" | "[^"))
            {
                self.bracket_character(character, value);
                continue;
            }
            if self.quote && character != '"' {
                let escaped = escape_regex(&value);
                self.previous_mut().value.push_str(&escaped);
                self.output.push_str(&escaped);
                continue;
            }
            match character {
                '"' => self.quote = !self.quote,
                '(' => {
                    self.parens += 1;
                    self.stack.push(Kind::Paren);
                    self.push(Token::literal(Kind::Paren, value));
                }
                ')' => {
                    if self
                        .extglobs
                        .last()
                        .is_some_and(|token| self.parens == token.parens + 1)
                    {
                        let token = self.extglobs.pop().expect("checked above");
                        self.close_extglob(token);
                    } else {
                        self.push(Token::new(
                            Kind::Paren,
                            value,
                            if self.parens == 0 { r"\)" } else { ")" },
                        ));
                        self.parens = self.parens.saturating_sub(1);
                        self.stack.pop();
                    }
                }
                '[' => {
                    if self.input[self.index..].contains(&']') {
                        self.brackets += 1;
                        self.stack.push(Kind::Bracket);
                        self.push(Token::literal(Kind::Bracket, value));
                    } else {
                        self.push(Token::literal(Kind::Bracket, r"\["));
                    }
                }
                ']' => self.close_bracket(),
                '{' => {
                    let brace = Brace {
                        token: self.tokens.len(),
                        output_start: self.output.len(),
                        dots: false,
                        comma: false,
                    };
                    self.stack.push(Kind::Brace);
                    self.push(Token::new(Kind::Brace, value, "("));
                    self.braces.push(brace);
                }
                '}' => self.close_brace(),
                ',' => {
                    let mut output = ",";
                    if self.stack.last() == Some(&Kind::Brace)
                        && let Some(brace) = self.braces.last_mut()
                    {
                        brace.comma = true;
                        output = "|";
                    }
                    self.push(Token::new(Kind::Comma, value, output));
                }
                '/' => {
                    if self.previous().kind == Kind::Dot && self.index == self.start + 2 {
                        self.start = self.index;
                        self.output.clear();
                        self.tokens.pop();
                    } else {
                        self.push(Token::new(Kind::Slash, value, SLASH));
                    }
                }
                '.' => {
                    if !self.braces.is_empty() && self.previous().kind == Kind::Dot {
                        let previous = self.previous_mut();
                        if previous.value == "." {
                            r"\.".clone_into(&mut previous.output);
                        }
                        previous.kind = Kind::Dots;
                        previous.output.push('.');
                        previous.value.push('.');
                        self.braces.last_mut().expect("checked above").dots = true;
                    } else {
                        let kind = if self.braces.is_empty()
                            && self.parens == 0
                            && !matches!(self.previous().kind, Kind::Bos | Kind::Slash)
                        {
                            Kind::Text
                        } else {
                            Kind::Dot
                        };
                        self.push(Token::new(kind, value, r"\."));
                    }
                }
                '?' => self.question_mark(),
                '!' => {
                    if self.peek(0) == Some('(')
                        && (self.peek(1) != Some('?')
                            || !self
                                .peek(2)
                                .is_some_and(|character| "!=<:".contains(character)))
                    {
                        self.open_extglob(Kind::Negate, '!');
                    } else if self.index == 1 {
                        let mut count = 1;
                        while self.peek(0) == Some('!')
                            && (self.peek(1) != Some('(') || self.peek(2) == Some('?'))
                        {
                            self.index += 1;
                            self.start += 1;
                            count += 1;
                        }
                        if count % 2 != 0 {
                            self.negated = true;
                            self.start += 1;
                        }
                    } else {
                        self.text(value);
                    }
                }
                '+' => {
                    if self.peek(0) == Some('(') && self.peek(1) != Some('?') {
                        self.open_extglob(Kind::Plus, '+');
                    } else if self.previous().value == "(" {
                        self.push(Token::new(Kind::Plus, value, r"\+"));
                    } else if matches!(
                        self.previous().kind,
                        Kind::Bracket | Kind::Paren | Kind::Brace
                    ) || self.parens > 0
                    {
                        self.push(Token::literal(Kind::Plus, value));
                    } else {
                        self.push(Token::literal(Kind::Plus, r"\+"));
                    }
                }
                '@' if self.peek(0) == Some('(') && self.peek(1) != Some('?') => {
                    let mut token = Token::new(Kind::At, value, "");
                    token.extglob = true;
                    self.push(token);
                }
                '|' | '@' => self.push(Token::literal(Kind::Text, value)),
                '*' => self.star(),
                '$' | '^' => self.text(format!("\\{value}")),
                _ => self.text(value),
            }
        }
        for _ in 0..self.brackets {
            escape_last(&mut self.output, '[');
        }
        for _ in 0..self.parens {
            escape_last(&mut self.output, '(');
        }
        for _ in 0..self.braces.len() {
            escape_last(&mut self.output, '{');
        }
        if matches!(self.previous().kind, Kind::Star | Kind::Bracket) {
            self.push(Token::new(Kind::Text, "", r"\/?"));
        }
        if self.backtrack {
            self.output = self
                .tokens
                .iter()
                .map(|token| token.output.as_str())
                .collect();
        }
        Parsed {
            output: self.output,
            negated: self.negated,
        }
    }

    fn text(&mut self, mut value: String) {
        while self
            .peek(0)
            .is_some_and(|character| !"@![].,$*+?^{}()|\\/".contains(character))
        {
            value.push(self.peek(0).expect("checked above"));
            self.index += 1;
        }
        self.push(Token::literal(Kind::Text, value));
    }

    fn bracket_character(&mut self, character: char, mut value: String) {
        if character == ':' && self.previous().value[1..].contains('[') {
            self.previous_mut().posix = true;
            if self.previous().value[1..].contains(':') {
                let position = self.previous().value.rfind('[').expect("checked above");
                let name = &self.previous().value[position + 2..];
                if let Some(source) = posix_class(name) {
                    self.previous_mut().value.truncate(position);
                    self.previous_mut().value.push_str(source);
                    let value = self.previous().value.clone();
                    self.previous_mut().output = value;
                    self.backtrack = true;
                    self.index += 1;
                    if self.tokens[0].output.is_empty() && self.tokens.len() == 2 {
                        ONE.clone_into(&mut self.tokens[0].output);
                    }
                    return;
                }
            }
        }
        if (character == '[' && self.peek(0) != Some(':'))
            || (character == '-' && self.peek(0) == Some(']'))
            || (character == ']' && matches!(self.previous().value.as_str(), "[" | "[^"))
        {
            value.insert(0, '\\');
        }
        self.previous_mut().value.push_str(&value);
        self.previous_mut().output.push_str(&value);
        self.output.push_str(&value);
    }

    fn close_bracket(&mut self) {
        if self.brackets == 0 {
            self.push(Token::new(Kind::Text, "]", r"\]"));
            return;
        }
        self.brackets -= 1;
        self.stack.pop();
        let previous = self.previous();
        let inner = &previous.value[1..];
        let value = if !previous.posix && inner.starts_with('^') && !inner.contains('/') {
            "/]"
        } else {
            "]"
        };
        self.previous_mut().value.push_str(value);
        self.previous_mut().output.push_str(value);
        self.output.push_str(value);
        let previous = self.previous();
        if previous.value[1..previous.value.len() - value.len()]
            .chars()
            .any(regex_special)
        {
            return;
        }
        let escaped = escape_regex(&previous.value);
        self.output
            .truncate(self.output.len().saturating_sub(previous.value.len()));
        let expanded = format!("(?:{escaped}|{})", self.previous().value);
        self.previous_mut().value.clone_from(&expanded);
        self.previous_mut().output.clone_from(&expanded);
        self.output.push_str(&expanded);
    }

    fn close_brace(&mut self) {
        let Some(brace) = self.braces.pop() else {
            self.push(Token::literal(Kind::Text, "}"));
            return;
        };
        let mut output = ")".to_owned();
        let mut value = "}".to_owned();
        if brace.dots {
            let mut range = Vec::new();
            while let Some(token) = self.tokens.pop() {
                if token.kind == Kind::Brace {
                    break;
                }
                if token.kind != Kind::Dots {
                    range.push(token.value);
                }
            }
            range.sort();
            output = format!("[{}]", range.join("-"));
            if Regex::new(&output).is_err() {
                output = range
                    .iter()
                    .map(|part| escape_regex(part))
                    .collect::<Vec<_>>()
                    .join("..");
            }
            self.backtrack = true;
        }
        if !brace.comma && !brace.dots {
            self.output.truncate(brace.output_start);
            r"\{".clone_into(&mut self.tokens[brace.token].value);
            r"\{".clone_into(&mut self.tokens[brace.token].output);
            r"\}".clone_into(&mut value);
            output.clone_from(&value);
            for token in &self.tokens[brace.token..] {
                self.output.push_str(if token.output.is_empty() {
                    &token.value
                } else {
                    &token.output
                });
            }
        }
        self.push(Token::new(Kind::Brace, value, output));
        self.stack.pop();
    }

    fn question_mark(&mut self) {
        if self.previous().value != "(" && self.peek(0) == Some('(') && self.peek(1) != Some('?') {
            self.open_extglob(Kind::Qmark, '?');
        } else if self.previous().kind == Kind::Paren {
            let next = self.peek(0);
            let invalid_group =
                self.previous().value == "(" && !next.is_some_and(|value| "!=<:".contains(value));
            let invalid_name = next == Some('<')
                && !self.remaining()[1..].starts_with(['!', '='])
                && !self.remaining()[1..]
                    .split_once('>')
                    .is_some_and(|(name, _)| !name.is_empty() && name.chars().all(is_word));
            self.push(Token::new(
                Kind::Text,
                "?",
                if invalid_group || invalid_name {
                    r"\?"
                } else {
                    "?"
                },
            ));
        } else {
            self.push(Token::new(
                Kind::Qmark,
                "?",
                if matches!(self.previous().kind, Kind::Slash | Kind::Bos) {
                    r"[^.\/]"
                } else {
                    "[^/]"
                },
            ));
        }
    }

    fn star(&mut self) {
        if self.previous().kind == Kind::Globstar || self.previous().star {
            let previous = self.previous_mut();
            previous.kind = Kind::Star;
            previous.star = true;
            previous.value.push('*');
            STAR.clone_into(&mut previous.output);
            self.backtrack = true;
            return;
        }
        if self.peek(0) == Some('(') && self.peek(1) != Some('?') && self.peek(1).is_some() {
            self.open_extglob(Kind::Star, '*');
            return;
        }
        if self.previous().kind == Kind::Star {
            let previous_index = self.tokens.len() - 1;
            let prior_index = self.previous().prev;
            let prior = &self.tokens[prior_index];
            let before = &self.tokens[prior.prev];
            let is_start = matches!(prior.kind, Kind::Slash | Kind::Bos);
            let after_star = matches!(before.kind, Kind::Star | Kind::Globstar);
            let is_brace =
                !self.braces.is_empty() && matches!(prior.kind, Kind::Comma | Kind::Brace);
            let is_extglob = !self.extglobs.is_empty() && prior.kind == Kind::Paren;
            if !is_start && prior.kind != Kind::Paren && !is_brace && !is_extglob {
                self.push(Token::new(Kind::Star, "*", ""));
                return;
            }
            while self.remaining().starts_with("/**")
                && self.peek(3).is_none_or(|character| character == '/')
            {
                self.index += 3;
            }
            let at_end = self.index == self.input.len();
            if self.tokens[prior_index].kind == Kind::Bos && at_end {
                let previous = self.previous_mut();
                previous.kind = Kind::Globstar;
                previous.value.push('*');
                GLOBSTAR.clone_into(&mut previous.output);
                GLOBSTAR.clone_into(&mut self.output);
                return;
            }
            let middle_slash = self.tokens[prior_index].kind == Kind::Slash
                && self.tokens[self.tokens[prior_index].prev].kind != Kind::Bos;
            if middle_slash && !after_star && at_end {
                self.output.truncate(self.output.len().saturating_sub(
                    self.tokens[prior_index].output.len()
                        + self.tokens[previous_index].output.len(),
                ));
                self.tokens[prior_index].output = format!("(?:{}", self.tokens[prior_index].output);
                self.tokens[previous_index].kind = Kind::Globstar;
                self.tokens[previous_index].output = format!("{GLOBSTAR}|$)");
                self.tokens[previous_index].value.push('*');
                self.output.push_str(&self.tokens[prior_index].output);
                self.output.push_str(&self.tokens[previous_index].output);
                return;
            }
            if middle_slash && self.peek(0) == Some('/') {
                let end = if self.peek(1).is_some() { "|$" } else { "" };
                self.output.truncate(self.output.len().saturating_sub(
                    self.tokens[prior_index].output.len()
                        + self.tokens[previous_index].output.len(),
                ));
                self.tokens[prior_index].output = format!("(?:{}", self.tokens[prior_index].output);
                self.tokens[previous_index].kind = Kind::Globstar;
                self.tokens[previous_index].output = format!("{GLOBSTAR}{SLASH}|{SLASH}{end})");
                self.tokens[previous_index].value.push('*');
                self.output.push_str(&self.tokens[prior_index].output);
                self.output.push_str(&self.tokens[previous_index].output);
                self.index += 1;
                self.push(Token::new(Kind::Slash, "/", ""));
                return;
            }
            if self.tokens[prior_index].kind == Kind::Bos && self.peek(0) == Some('/') {
                let output = format!("(?:^|{SLASH}|{GLOBSTAR}{SLASH})");
                self.tokens[previous_index].kind = Kind::Globstar;
                self.tokens[previous_index].value.push('*');
                self.tokens[previous_index].output.clone_from(&output);
                self.output = output;
                self.index += 1;
                self.push(Token::new(Kind::Slash, "/", ""));
                return;
            }
            self.output.truncate(
                self.output
                    .len()
                    .saturating_sub(self.previous().output.len()),
            );
            let previous = self.previous_mut();
            previous.kind = Kind::Globstar;
            GLOBSTAR.clone_into(&mut previous.output);
            previous.value.push('*');
            self.output.push_str(GLOBSTAR);
            return;
        }
        if self.index - 1 == self.start || matches!(self.previous().kind, Kind::Slash | Kind::Dot) {
            let prefix = if self.previous().kind == Kind::Dot {
                NO_DOT_SLASH
            } else {
                NO_DOT
            };
            self.output.push_str(prefix);
            self.previous_mut().output.push_str(prefix);
            if self.peek(0) != Some('*') {
                self.output.push_str(ONE);
                self.previous_mut().output.push_str(ONE);
            }
        }
        self.push(Token::new(Kind::Star, "*", STAR));
    }
}

fn simple_fast_path(input: &[char]) -> String {
    let mut output = String::new();
    let mut index = 0;
    while index < input.len() {
        if is_word(input[index]) {
            output.push(input[index]);
            index += 1;
            continue;
        }
        let escaped =
            input[index] == '\\' && input.get(index + 1).is_some_and(|value| !is_word(*value));
        if escaped {
            index += 1;
        }
        let character = input[index];
        let mut length = 1;
        while input.get(index + length) == Some(&character) {
            length += 1;
        }
        match character {
            '\\' => output.extend(std::iter::repeat_n('\\', length + usize::from(escaped))),
            '?' => {
                if escaped {
                    output.push_str(r"\?");
                    output.push_str(&"[^/]".repeat(length - 1));
                } else if index == 0 {
                    output.push_str(r"[^.\/]");
                    output.push_str(&"[^/]".repeat(length - 1));
                } else {
                    output.push_str(&"[^/]".repeat(length));
                }
            }
            '.' => output.push_str(&r"\.".repeat(length)),
            '*' => {
                if escaped {
                    output.push_str(r"\*");
                    if length > 1 {
                        output.push_str(STAR);
                    }
                } else {
                    output.push_str(STAR);
                }
            }
            _ => {
                output.push('\\');
                output.extend(std::iter::repeat_n(character, length));
            }
        }
        index += length;
    }
    let mut normalized = String::new();
    let mut characters = output.chars().peekable();
    while let Some(character) = characters.next() {
        normalized.push(character);
        if character == '\\' {
            let mut count = 1;
            while characters.peek() == Some(&'\\') {
                characters.next();
                count += 1;
            }
            if count % 2 == 0 {
                normalized.push('\\');
            }
        }
    }
    normalized
}

fn posix_class(name: &str) -> Option<&'static str> {
    Some(match name {
        "alnum" => "a-zA-Z0-9",
        "alpha" => "a-zA-Z",
        "ascii" => r"\x00-\x7F",
        "blank" => " \\t",
        "cntrl" => r"\x00-\x1F\x7F",
        "digit" => "0-9",
        "graph" => r"\x21-\x7E",
        "lower" => "a-z",
        "print" => r"\x20-\x7E ",
        "punct" => r##"\-!"#$%&'()\*+,./:;<=>?@[\]^_`{|}~"##,
        "space" => " \\t\\r\\n\\v\\f",
        "upper" => "A-Z",
        "word" => "A-Za-z0-9_",
        "xdigit" => "A-Fa-f0-9",
        _ => return None,
    })
}

fn branches(body: &str) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut bracket = 0usize;
    let mut paren = 0usize;
    let mut quote = false;
    let mut escaped = false;
    for character in body.chars() {
        if escaped {
            escaped = false;
        } else {
            match character {
                '\\' => escaped = true,
                '"' => quote = !quote,
                '[' if !quote => bracket += 1,
                ']' if !quote => bracket = bracket.saturating_sub(1),
                '(' if !quote && bracket == 0 => paren += 1,
                ')' if !quote && bracket == 0 => paren = paren.saturating_sub(1),
                '|' if !quote && bracket == 0 && paren == 0 => {
                    parts.push(String::new());
                    continue;
                }
                _ => {}
            }
        }
        parts.last_mut().expect("initialized above").push(character);
    }
    parts
}

fn simple_branch(branch: &str) -> Option<String> {
    let mut value = branch.trim();
    while value.starts_with("@(")
        && value.ends_with(')')
        && !value[2..value.len() - 1]
            .chars()
            .any(|character| "\\()[]{}|".contains(character))
    {
        value = &value[2..value.len() - 1];
    }
    let mut escaped = false;
    let mut output = String::new();
    for character in value.chars() {
        if escaped {
            output.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if "?*+@!()[]{}".contains(character) {
            return None;
        } else {
            output.push(character);
        }
    }
    if escaped {
        output.push('\\');
    }
    Some(output)
}

fn repeated_pattern(pattern: &str) -> Option<(char, &str, usize)> {
    let kind = pattern.chars().next()?;
    if !matches!(kind, '+' | '*')
        || !pattern.starts_with(['+', '*'])
        || pattern.as_bytes().get(1) != Some(&b'(')
    {
        return None;
    }
    let mut bracket = 0usize;
    let mut paren = 0usize;
    let mut quote = false;
    let mut escaped = false;
    for (position, character) in pattern.char_indices().skip(1) {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' => escaped = true,
            '"' => quote = !quote,
            '[' if !quote => bracket += 1,
            ']' if !quote => bracket = bracket.saturating_sub(1),
            '(' if !quote && bracket == 0 => paren += 1,
            ')' if !quote && bracket == 0 => {
                paren = paren.saturating_sub(1);
                if paren == 0 {
                    return Some((kind, &pattern[2..position], position));
                }
            }
            _ => {}
        }
    }
    None
}

#[expect(
    clippy::option_option,
    reason = "The parser distinguishes normal handling, literal fallback, and a replacement expression."
)]
fn repeated_extglob(body: &str) -> Option<Option<String>> {
    let alternatives = branches(body);
    if alternatives.len() > 1 {
        if alternatives.iter().any(|value| {
            value.trim().is_empty()
                || value
                    .trim()
                    .chars()
                    .all(|character| matches!(character, '*' | '?'))
        }) {
            return Some(None);
        }
        let plain: Vec<_> = alternatives
            .iter()
            .filter_map(|value| simple_branch(value))
            .filter(|value| !value.is_empty())
            .collect();
        for (index, left) in plain.iter().enumerate() {
            for right in &plain[index + 1..] {
                let character = left.chars().next().expect("empty values removed");
                if left.chars().all(|value| value == character)
                    && right.chars().all(|value| value == character)
                {
                    return Some(None);
                }
            }
        }
    }
    for alternative in alternatives {
        let mut remaining = alternative.trim();
        let mut sequence = Vec::new();
        while let Some(('*', inner, end)) = repeated_pattern(remaining) {
            if let Some(value) = simple_branch(inner)
                && value.chars().count() == 1
                && branches(inner).len() == 1
            {
                sequence.push(value);
                remaining = &remaining[end + 1..];
            } else {
                break;
            }
        }
        if remaining.is_empty() && !sequence.is_empty() {
            let source = if sequence.len() == 1 {
                escape_regex(&sequence[0])
            } else {
                format!(
                    "[{}]",
                    sequence
                        .iter()
                        .map(|value| escape_regex(value))
                        .collect::<String>()
                )
            };
            return Some(Some(source + "*"));
        }
        if repeated_pattern(alternative.trim())
            .is_some_and(|(_, _, end)| end == alternative.trim().len() - 1)
        {
            return Some(None);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_do_not_match_unrelated_substrings() {
        let matcher = IgnoreMatcher::new(&[
            "**/node_modules".into(),
            "**/.*".into(),
            "cache".into(),
            "data".into(),
        ])
        .unwrap();
        for path in [
            "node_modules",
            "pkg/node_modules",
            ".git",
            "pkg/.cache",
            "cache",
            "data",
        ] {
            assert!(matcher.is_match(path), "{path}");
        }
        for path in [
            "node_modules-copy",
            "src/cache.rs",
            "cache/file",
            "database",
            "pkg/data",
            "src/module.rs",
        ] {
            assert!(!matcher.is_match(path), "{path}");
        }
    }
}
