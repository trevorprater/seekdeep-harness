//! String helpers compatible with the vendored `CosmoKit` behavior.

use std::sync::OnceLock;

/// Capitalizes the first Unicode scalar without changing the remainder.
#[must_use]
pub fn capitalize(value: &str) -> String {
    let mut chars = value.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// Lowercases the first Unicode scalar without changing the remainder.
#[must_use]
pub fn uncapitalize(value: &str) -> String {
    let mut chars = value.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_lowercase().chain(chars).collect()
    })
}

/// Converts ASCII dash or underscore delimited text to camel case.
#[must_use]
pub fn camel_case(value: &str) -> String {
    let characters = value.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(value.len());
    let mut index = 0;
    while let Some(character) = characters.get(index).copied() {
        if matches!(character, '-' | '_')
            && characters
                .get(index + 1)
                .is_some_and(char::is_ascii_lowercase)
        {
            output.push(characters[index + 1].to_ascii_uppercase());
            index += 2;
        } else {
            output.push(character);
            index += 1;
        }
    }
    output
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TokenState {
    Delimiter,
    Upper,
    Lower,
}

fn tokenize(source: &str, delimiter: char) -> String {
    let characters = source.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(source.len());
    let mut state = TokenState::Delimiter;
    for (index, character) in characters.iter().copied().enumerate() {
        if character.is_ascii_uppercase() {
            if state == TokenState::Upper {
                if characters
                    .get(index + 1)
                    .is_some_and(char::is_ascii_lowercase)
                {
                    output.push(delimiter);
                }
                output.push(character.to_ascii_lowercase());
            } else {
                if state != TokenState::Delimiter {
                    output.push(delimiter);
                }
                output.push(character.to_ascii_lowercase());
            }
            state = TokenState::Upper;
        } else if character.is_ascii_lowercase() {
            output.push(character);
            state = TokenState::Lower;
        } else if matches!(character, '-' | '_') {
            if state != TokenState::Delimiter {
                output.push(delimiter);
            }
            state = TokenState::Delimiter;
        } else {
            output.push(character);
        }
    }
    output
}

/// Converts text to dash-delimited parameter case.
#[must_use]
pub fn param_case(value: &str) -> String {
    tokenize(value, '-')
}

/// Converts text to underscore-delimited snake case.
#[must_use]
pub fn snake_case(value: &str) -> String {
    tokenize(value, '_')
}

/// Formats a string key as a JavaScript member-access suffix.
///
/// # Panics
///
/// Panics only if the compile-time constant identifier expression is invalid.
#[must_use]
pub fn format_property(key: &str) -> String {
    static IDENTIFIER: OnceLock<regex::Regex> = OnceLock::new();
    let identifier = IDENTIFIER.get_or_init(|| {
        regex::Regex::new(r"(?i)^[a-z_$][a-z0-9_$]*$").expect("static property regex is valid")
    });
    if identifier.is_match(key) {
        format!(".{key}")
    } else {
        format!(
            "[{}]",
            serde_json::to_string(key).expect("serializing a string cannot fail")
        )
    }
}

/// Removes exactly one trailing slash.
#[must_use]
pub fn trim_slash(value: &str) -> String {
    value.strip_suffix('/').unwrap_or(value).to_owned()
}

/// Ensures a path begins with `/` and has no trailing slash.
#[must_use]
pub fn sanitize(value: &str) -> String {
    let value = if value.starts_with('/') {
        value.to_owned()
    } else {
        format!("/{value}")
    };
    trim_slash(&value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenization_matches_acronym_boundaries() {
        assert_eq!(param_case("XMLHttpRequest"), "xml-http-request");
        assert_eq!(snake_case("foo-Bar_BAZ"), "foo_bar_baz");
        assert_eq!(param_case("foo--bar"), "foo-bar");
    }

    #[test]
    fn camel_case_only_consumes_delimiter_before_lowercase() {
        assert_eq!(camel_case("foo-bar_baz"), "fooBarBaz");
        assert_eq!(camel_case("foo-1"), "foo-1");
    }

    #[test]
    fn paths_match_single_trailing_slash_behavior() {
        assert_eq!(sanitize(""), "");
        assert_eq!(sanitize("foo/"), "/foo");
        assert_eq!(trim_slash("foo//"), "foo/");
    }

    #[test]
    fn property_formatting_keeps_javascript_ascii_identifier_rules() {
        assert_eq!(format_property("alpha_$1"), ".alpha_$1");
        assert_eq!(format_property("é"), "[\"é\"]");
        assert_eq!(format_property("bad-key"), "[\"bad-key\"]");
    }
}
