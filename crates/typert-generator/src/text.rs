//! Source-compatible generated identifier, quoting, and collation helpers.

use std::{cmp::Ordering, sync::OnceLock};

use icu_collator::{Collator, CollatorBorrowed, CollatorPreferences, options::CollatorOptions};
use icu_locale::Locale;

pub(crate) fn quote(value: &str) -> String {
    format!(
        "'{}'",
        value
            .replace('\\', "\\\\")
            .replace('\'', "\\'")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
    )
}

pub(crate) fn safe_identifier(value: &str) -> String {
    let mut normalized: String = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '$') {
                character
            } else {
                '_'
            }
        })
        .collect();
    if !normalized
        .as_bytes()
        .first()
        .is_some_and(|first| first.is_ascii_alphabetic() || matches!(first, b'_' | b'$'))
    {
        normalized.insert(0, '_');
    }
    normalized
}

pub(crate) fn indent(value: &str, spaces: usize) -> String {
    let prefix = " ".repeat(spaces);
    value
        .split('\n')
        .map(|line| format!("{prefix}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn locale_compare(left: &str, right: &str) -> Ordering {
    static COLLATOR: OnceLock<CollatorBorrowed<'static>> = OnceLock::new();
    COLLATOR
        .get_or_init(|| {
            let locale = sys_locale::get_locale()
                .and_then(|locale| locale.parse::<Locale>().ok())
                .unwrap_or_else(|| "en-US".parse().expect("valid fallback locale"));
            Collator::try_new(
                CollatorPreferences::from(&locale),
                CollatorOptions::default(),
            )
            .expect("compiled locale collation data")
        })
        .compare(left, right)
}

pub(crate) fn utf16_compare(left: &str, right: &str) -> Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}
