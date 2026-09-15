use super::policy;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Token<'a> {
    And,
    Or,
    With,
    Left,
    Right,
    Colon,
    Plus,
    License(&'a str),
    Exception,
    DocumentRef,
    LicenseRef,
}

fn id_string(source: &str) -> Option<&str> {
    let start = source.find(|character: char| {
        character.is_ascii_alphanumeric() || matches!(character, '-' | '.')
    })?;
    let text = &source[start..];
    let end = text
        .find(|character: char| {
            !character.is_ascii_alphanumeric() && !matches!(character, '-' | '.')
        })
        .unwrap_or(text.len());
    Some(&text[..end])
}

fn scan(source: &str) -> Option<Vec<Token<'_>>> {
    let mut index = 0;
    let mut tokens = Vec::new();
    while index < source.len() {
        while source.as_bytes().get(index) == Some(&b' ') {
            index += 1;
        }
        if index == source.len() {
            break;
        }
        let tail = source.get(index..)?;
        let mut found = None;
        for (operator, token) in [
            ("WITH", Token::With),
            ("AND", Token::And),
            ("OR", Token::Or),
            ("(", Token::Left),
            (")", Token::Right),
            (":", Token::Colon),
            ("+", Token::Plus),
        ] {
            if tail
                .get(..operator.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(operator))
            {
                if token == Token::Plus && index > 0 && source.as_bytes()[index - 1] == b' ' {
                    return None;
                }
                index += operator.len();
                found = Some(token);
                break;
            }
        }
        if let Some(token) = found {
            tokens.push(token);
            continue;
        }
        let mut reference = false;
        for (prefix, token) in [
            ("DocumentRef-", Token::DocumentRef),
            ("LicenseRef-", Token::LicenseRef),
        ] {
            if let Some(rest) = tail.strip_prefix(prefix) {
                let id = id_string(rest)?;
                index += prefix.len() + id.len();
                tokens.push(token);
                reference = true;
                break;
            }
        }
        if reference {
            continue;
        }
        let id = id_string(tail)?;
        if policy().licenses.contains(id) {
            tokens.push(Token::License(id));
        } else if policy().exceptions.contains(id) {
            tokens.push(Token::Exception);
        } else {
            return None;
        }
        index += id.len();
    }
    Some(tokens)
}

struct Parser<'a> {
    tokens: &'a [Token<'a>],
    position: usize,
}

impl Parser<'_> {
    fn take(&mut self, token: Token<'_>) -> bool {
        if self.tokens.get(self.position) == Some(&token) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn atom(&mut self) -> Option<bool> {
        if self.take(Token::Left) {
            let value = self.expression()?;
            return self.take(Token::Right).then_some(value);
        }
        let mut permissive = match *self.tokens.get(self.position)? {
            Token::DocumentRef => {
                self.position += 1;
                if !self.take(Token::Colon) || !self.take(Token::LicenseRef) {
                    return None;
                }
                false
            }
            Token::LicenseRef => {
                self.position += 1;
                false
            }
            Token::License(license) => {
                self.position += 1;
                let plus = self.take(Token::Plus);
                !plus && policy().permissive_licenses.contains(license)
            }
            _ => return None,
        };
        if self.take(Token::With) {
            if !self.take(Token::Exception) {
                return None;
            }
            permissive = false;
        }
        Some(permissive)
    }

    fn conjunction(&mut self) -> Option<bool> {
        let mut value = self.atom()?;
        while self.take(Token::And) {
            value &= self.atom()?;
        }
        Some(value)
    }

    fn expression(&mut self) -> Option<bool> {
        let mut value = self.conjunction()?;
        while self.take(Token::Or) {
            value |= self.conjunction()?;
        }
        Some(value)
    }
}

/// Applies the pinned permissive-license policy to a complete SPDX expression.
/// Unknown tokens, malformed expressions, additions and exception clauses fail closed.
#[must_use]
pub fn is_permissive(license: &str) -> bool {
    let normalized = license
        .split('/')
        .map(|part| part.trim_matches(crate::jsdoc::is_js_space))
        .collect::<Vec<_>>()
        .join(" OR ");
    let Some(tokens) = scan(&normalized) else {
        return false;
    };
    let mut parser = Parser {
        tokens: &tokens,
        position: 0,
    };
    parser
        .expression()
        .is_some_and(|permissive| permissive && parser.position == tokens.len())
}
